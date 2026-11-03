//! Mailboxes: ordered, bidirectional channels of opaque pairing messages
//! between exactly two peers. The SPAKE2 + sealed `PeerInfo` exchange
//! (`crate::exchange`) runs over any of them.
//!
//! Implementations:
//! - [`crate::rendezvous::RendezvousMailbox`]: the rendezvous server's `Relay`.
//! - [`crate::lan::LanMailbox`]: a direct TCP connection found via mDNS.
//! - [`crate::dht::DhtMailbox`]: BEP 44 mutable items on the Mainline DHT.
//!
//! The sender races all of its mailboxes in `PendingSend::wait_for_peer`; the
//! receiver tries them in `join`.

use std::future::Future;

use peerly_proto::pairing::PairingMsg;

use crate::PairingError;

pub(crate) trait Mailbox: Send {
    /// Delivers one message to the peer.
    fn send(&mut self, msg: &PairingMsg) -> impl Future<Output = Result<(), PairingError>> + Send;

    /// Next message from the peer. No timeout of its own: the caller bounds each step.
    fn recv(&mut self) -> impl Future<Output = Result<PairingMsg, PairingError>> + Send;

    /// Best-effort graceful close once the exchange is over (or failed).
    fn close(self) -> impl Future<Output = ()> + Send;

    /// True if the first `recv` is "wait for the peer to show up" (DHT sender):
    /// it then has no timeout, and the local socket/STUN preparation is deferred
    /// until it returns so the NAT mapping is fresh. Other mailboxes wait for
    /// the peer before the exchange starts.
    fn waits_for_peer(&self) -> bool {
        false
    }
}
