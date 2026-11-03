//! Serverless internet pairing over the BitTorrent Mainline DHT
//! (`peerly_proto::dht`): BEP 44 mutable items, one signing key per nameplate.
//!
//! The key is derived from the nameplate only and is public on purpose: anyone
//! can read and write the slots. Nothing derived from the code words is ever
//! published besides SPAKE2 messages and sealed `PeerInfo`, so the DHT gives an
//! attacker no offline guessing material. Slots: sender `s1` (Pake), `s2`
//! (Sealed); receiver `r1` (Pake), `r2` (Sealed). Every item carries the
//! sender's random session id; leftovers from older sessions are ignored.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use mainline::async_dht::AsyncDht;
use mainline::errors::{PutMutableError, PutQueryError};
use mainline::{Dht, MutableItem, SigningKey};
use peerly_proto::PROTOCOL_VERSION;
use peerly_proto::dht::{
    DhtItem, ITEM_MAX_AGE_SECS, KEY_DOMAIN, SALT_RECEIVER_PAKE, SALT_RECEIVER_SEALED,
    SALT_SENDER_PAKE, SALT_SENDER_SEALED,
};
use peerly_proto::pairing::PairingMsg;
use sha2::{Digest, Sha256};

use crate::mailbox::Mailbox;
use crate::seal::Role;
use crate::{EXCHANGE_TIMEOUT, PairingError};

/// Locally generated nameplates tried before giving up on the DHT.
pub(crate) const MAX_NAMEPLATE_TRIES: usize = 5;
/// Upper bound for the sender's "is this nameplate taken?" lookup.
const COLLISION_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll backoff: first pause, growing by a second up to the max.
const POLL_FIRST: Duration = Duration::from_secs(1);
const POLL_MAX: Duration = Duration::from_secs(3);
/// Tolerated clock skew for items "created in the future".
const FUTURE_SLACK_SECS: u64 = 120;
/// Put attempts when the node has not found any DHT nodes yet.
const PUT_ATTEMPTS: u32 = 4;
/// BEP 44 limit for a mutable item's value.
const MAX_VALUE_LEN: usize = 1000;

const SENDER_SLOTS: [&[u8]; 2] = [SALT_SENDER_PAKE, SALT_SENDER_SEALED];
const RECEIVER_SLOTS: [&[u8]; 2] = [SALT_RECEIVER_PAKE, SALT_RECEIVER_SEALED];

fn dht_err(e: impl std::fmt::Display) -> PairingError {
    PairingError::Io(std::io::Error::other(format!("DHT: {e}")))
}

fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Ed25519 key for a nameplate: seed = SHA-256(KEY_DOMAIN || decimal nameplate).
pub(crate) fn signing_key(nameplate: u32) -> SigningKey {
    let mut h = Sha256::new();
    h.update(KEY_DOMAIN);
    h.update(nameplate.to_string().as_bytes());
    SigningKey::from_bytes(&h.finalize().into())
}

/// Decodes and validates one slot value: right version, fresh, and (if
/// given) the expected session.
pub(crate) fn accept_item(
    value: &[u8],
    session: Option<&[u8; 16]>,
    now_secs: u64,
) -> Option<DhtItem> {
    let item: DhtItem = peerly_proto::decode(value).ok()?;
    let fresh = item.created.saturating_add(ITEM_MAX_AGE_SECS) >= now_secs
        && item.created <= now_secs.saturating_add(FUTURE_SLACK_SECS);
    let ours = session.is_none_or(|s| *s == item.session);
    (item.version == PROTOCOL_VERSION && fresh && ours).then_some(item)
}

/// A DHT client node. Cheap to clone; the node thread stops when the last
/// clone is dropped.
#[derive(Clone)]
pub(crate) struct DhtNode {
    dht: AsyncDht,
    /// Number of lookups started (observability; tests check polling stops).
    gets: Arc<AtomicUsize>,
}

impl DhtNode {
    /// `bootstrap = None` joins the public Mainline DHT.
    pub(crate) fn start(
        bootstrap: Option<&[String]>,
        gets: Arc<AtomicUsize>,
    ) -> Result<Self, PairingError> {
        let mut b = Dht::builder();
        // Random port: the default 6881 is often held by BitTorrent clients
        // (or a second Peerly instance), which silently breaks queries.
        b.port(0);
        if let Some(nodes) = bootstrap {
            b.bootstrap(nodes);
        }
        let dht = b.build().map_err(dht_err)?.as_async();
        Ok(DhtNode { dht, gets })
    }

    /// One lookup round: the first acceptable item in `salt`, if any.
    async fn lookup(
        &self,
        public: &[u8; 32],
        salt: &[u8],
        session: Option<&[u8; 16]>,
    ) -> Option<DhtItem> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        let now_secs = now().as_secs();
        let mut items = self.dht.get_mutable(public, Some(salt), None);
        while let Some(item) = items.next().await {
            if let Some(d) = accept_item(item.value(), session, now_secs) {
                return Some(d);
            }
        }
        None
    }

    /// Repeats lookups with backoff until an acceptable item appears. No
    /// timeout of its own: callers bound it (or cancel by dropping).
    async fn poll(&self, public: &[u8; 32], salt: &[u8], session: Option<&[u8; 16]>) -> DhtItem {
        let mut pause = POLL_FIRST;
        loop {
            if let Some(item) = self.lookup(public, salt, session).await {
                return item;
            }
            tokio::time::sleep(pause).await;
            pause = (pause + Duration::from_secs(1)).min(POLL_MAX);
        }
    }

    pub(crate) async fn put(
        &self,
        signer: &SigningKey,
        salt: &[u8],
        item: &DhtItem,
    ) -> Result<(), PairingError> {
        let value =
            peerly_proto::encode(item).map_err(|e| PairingError::Protocol(e.to_string()))?;
        if value.len() > MAX_VALUE_LEN {
            return Err(PairingError::Protocol(format!(
                "DHT item too large: {} bytes",
                value.len()
            )));
        }
        let mut attempt = 1;
        loop {
            // Milliseconds since the epoch: always above the seq of older sessions' items.
            let seq = i64::try_from(now().as_millis()).unwrap_or(i64::MAX);
            let mutable = MutableItem::new(signer.clone(), &value, seq, Some(salt));
            match self.dht.put_mutable(mutable, None).await {
                Ok(_) => return Ok(()),
                // A fresh node may not know any nodes yet: wait for bootstrap, retry.
                Err(PutMutableError::Query(
                    PutQueryError::NoClosestNodes | PutQueryError::Timeout,
                )) if attempt < PUT_ATTEMPTS => {
                    tracing::debug!(attempt, "DHT put found no nodes yet, retrying");
                    // A lookup on the same target refreshes the storing nodes
                    // (and their write tokens) the put needs.
                    tokio::time::sleep(POLL_FIRST).await;
                    let public = signer.verifying_key().to_bytes();
                    self.dht
                        .get_mutable(&public, Some(salt), None)
                        .for_each(|_| async {})
                        .await;
                    attempt += 1;
                }
                Err(e) => return Err(dht_err(e)),
            }
        }
    }

    /// True if a fresh `s1` exists for `nameplate` (another sender is using it).
    /// A lookup that times out counts as free.
    pub(crate) async fn nameplate_taken(&self, nameplate: u32) -> bool {
        let public = signing_key(nameplate).verifying_key().to_bytes();
        tokio::time::timeout(
            COLLISION_CHECK_TIMEOUT,
            self.lookup(&public, SALT_SENDER_PAKE, None),
        )
        .await
        .ok()
        .flatten()
        .is_some()
    }
}

/// [`Mailbox`] over DHT slots. Messages are written to our own slots in
/// order and read from the peer's slots in order.
pub(crate) struct DhtMailbox {
    node: DhtNode,
    signer: SigningKey,
    public: [u8; 32],
    session: [u8; 16],
    role: Role,
    sent: usize,
    received: usize,
    /// Receiver: the sender's `s1` Pake, read while joining.
    early: Option<PairingMsg>,
}

impl DhtMailbox {
    fn new(node: DhtNode, nameplate: u32, session: [u8; 16], role: Role) -> Self {
        let signer = signing_key(nameplate);
        let public = signer.verifying_key().to_bytes();
        DhtMailbox {
            node,
            signer,
            public,
            session,
            role,
            sent: 0,
            received: 0,
            early: None,
        }
    }

    /// Sender: a new session on `nameplate` (nothing is written yet).
    pub(crate) fn offer(node: DhtNode, nameplate: u32) -> Self {
        let mut session = [0u8; 16];
        getrandom::fill(&mut session).expect("OS random number generator unavailable");
        Self::new(node, nameplate, session, Role::Sender)
    }

    /// Receiver: finds the sender's fresh `s1` (polling for up to one exchange
    /// step) and adopts its session. `CodeNotFound` if nothing shows up.
    pub(crate) async fn join(node: DhtNode, nameplate: u32) -> Result<Self, PairingError> {
        let public = signing_key(nameplate).verifying_key().to_bytes();
        let s1 = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
            loop {
                let item = node.poll(&public, SALT_SENDER_PAKE, None).await;
                if matches!(item.msg, PairingMsg::Pake(_)) {
                    return item;
                }
                tokio::time::sleep(POLL_FIRST).await;
            }
        })
        .await
        .map_err(|_| PairingError::CodeNotFound)?;
        tracing::debug!(%nameplate, "DHT: sender found");
        let mut mb = Self::new(node, nameplate, s1.session, Role::Receiver);
        mb.early = Some(s1.msg);
        mb.received = 1;
        Ok(mb)
    }

    fn slots(&self) -> (&'static [&'static [u8]; 2], &'static [&'static [u8]; 2]) {
        match self.role {
            Role::Sender => (&SENDER_SLOTS, &RECEIVER_SLOTS),
            Role::Receiver => (&RECEIVER_SLOTS, &SENDER_SLOTS),
        }
    }
}

impl Mailbox for DhtMailbox {
    async fn send(&mut self, msg: &PairingMsg) -> Result<(), PairingError> {
        let salt =
            *self.slots().0.get(self.sent).ok_or_else(|| {
                PairingError::Protocol("DHT mailbox: no slot left to write".into())
            })?;
        self.sent += 1;
        let item = DhtItem {
            version: PROTOCOL_VERSION,
            session: self.session,
            created: now().as_secs(),
            msg: msg.clone(),
        };
        self.node.put(&self.signer, salt, &item).await
    }

    async fn recv(&mut self) -> Result<PairingMsg, PairingError> {
        if let Some(m) = self.early.take() {
            return Ok(m);
        }
        let salt =
            *self.slots().1.get(self.received).ok_or_else(|| {
                PairingError::Protocol("DHT mailbox: no slot left to read".into())
            })?;
        self.received += 1;
        let item = self
            .node
            .poll(&self.public, salt, Some(&self.session))
            .await;
        Ok(item.msg)
    }

    /// Nothing to close: our items simply expire.
    async fn close(self) {}

    fn waits_for_peer(&self) -> bool {
        self.role == Role::Sender
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(session: [u8; 16], created: u64, version: u16) -> Vec<u8> {
        peerly_proto::encode(&DhtItem {
            version,
            session,
            created,
            msg: PairingMsg::Pake(vec![1, 2, 3]),
        })
        .unwrap()
    }

    #[test]
    fn key_depends_only_on_nameplate() {
        assert_eq!(signing_key(1234).to_bytes(), signing_key(1234).to_bytes());
        assert_ne!(signing_key(1234).to_bytes(), signing_key(1235).to_bytes());
        let mut h = Sha256::new();
        h.update(b"peerly/1/dht/1234");
        let seed: [u8; 32] = h.finalize().into();
        assert_eq!(signing_key(1234).to_bytes(), seed);
    }

    #[test]
    fn item_filtering() {
        let now = 1_000_000;
        let s = [7u8; 16];
        let v = PROTOCOL_VERSION;
        assert!(accept_item(&item(s, now, v), Some(&s), now).is_some());
        assert!(accept_item(&item(s, now, v), None, now).is_some());
        assert!(accept_item(&item(s, now - ITEM_MAX_AGE_SECS, v), Some(&s), now).is_some());
        // Stale, from the far future, other session, other version, garbage.
        assert!(accept_item(&item(s, now - ITEM_MAX_AGE_SECS - 1, v), Some(&s), now).is_none());
        assert!(accept_item(&item(s, now + 3600, v), Some(&s), now).is_none());
        assert!(accept_item(&item([8; 16], now, v), Some(&s), now).is_none());
        assert!(accept_item(&item(s, now, v + 1), Some(&s), now).is_none());
        assert!(accept_item(b"garbage", None, now).is_none());
    }
}
