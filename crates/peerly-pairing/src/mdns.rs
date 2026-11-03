//! mDNS discovery for LAN pairing (`peerly_proto::lan`). Kept separate from the
//! TCP mailbox in `lan.rs` so tests can bypass discovery.
//!
//! Both types own their own `ServiceDaemon` (a background thread) and shut it
//! down on drop, so an advertisement never outlives its `PendingSend`.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use peerly_proto::PROTOCOL_VERSION;
use peerly_proto::lan::{SERVICE_TYPE, TXT_NAMEPLATE, TXT_VERSION};
use tokio::time::Instant;

use crate::PairingError;

fn mdns_err(e: mdns_sd::Error) -> PairingError {
    PairingError::Io(std::io::Error::other(format!("mDNS: {e}")))
}

fn new_daemon() -> Result<ServiceDaemon, PairingError> {
    let daemon = ServiceDaemon::new().map_err(mdns_err)?;
    // IPv4 only in v1; skipping IPv6 interfaces avoids useless traffic and addresses.
    if let Err(e) = daemon.disable_interface(IfKind::IPv6) {
        tracing::debug!("mDNS: cannot disable IPv6 interfaces: {e}");
    }
    Ok(daemon)
}

fn random_hex() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS random number generator unavailable");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A registered `_peerly._tcp` service. Unregistered (goodbye packets) and the
/// daemon shut down when dropped.
pub(crate) struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    pub(crate) fn start(nameplate: u32, port: u16) -> Result<Self, PairingError> {
        let daemon = new_daemon()?;
        let id = random_hex();
        let instance = format!("peerly-{nameplate}-{id}");
        let host = format!("peerly-{id}.local.");
        let props = [
            (TXT_NAMEPLATE, nameplate.to_string()),
            (TXT_VERSION, PROTOCOL_VERSION.to_string()),
        ];
        let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", port, &props[..])
            .map_err(mdns_err)?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        let ad = Advertisement { daemon, fullname };
        ad.daemon.register(info).map_err(mdns_err)?;
        tracing::debug!(%nameplate, port, "mDNS advertisement registered");
        Ok(ad)
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
        tracing::debug!("mDNS advertisement removed");
    }
}

/// Browses `_peerly._tcp` until a deadline. Daemon shut down when dropped.
pub(crate) struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    seen: HashSet<String>,
    deadline: Instant,
}

impl Browser {
    pub(crate) fn start(timeout: Duration) -> Result<Self, PairingError> {
        let daemon = new_daemon()?;
        let events = daemon.browse(SERVICE_TYPE).map_err(mdns_err)?;
        Ok(Browser {
            daemon,
            events,
            seen: HashSet::new(),
            deadline: Instant::now() + timeout,
        })
    }

    /// Next not-yet-returned service advertising `nameplate` with our protocol
    /// version, as TCP addresses to try. Waits until the browse deadline; after
    /// it, only already-received events are returned.
    pub(crate) async fn next_match(&mut self, nameplate: u32) -> Option<Vec<SocketAddr>> {
        let np = nameplate.to_string();
        let version = PROTOCOL_VERSION.to_string();
        loop {
            let event = if Instant::now() < self.deadline {
                tokio::time::timeout_at(self.deadline, self.events.recv_async())
                    .await
                    .ok()?
                    .ok()?
            } else {
                self.events.try_recv().ok()?
            };
            let ServiceEvent::ServiceResolved(svc) = event else {
                continue;
            };
            if svc.get_property_val_str(TXT_NAMEPLATE) != Some(np.as_str())
                || svc.get_property_val_str(TXT_VERSION) != Some(version.as_str())
            {
                continue;
            }
            let port = svc.get_port();
            let mut ips: Vec<IpAddr> = svc
                .get_addresses_v4()
                .into_iter()
                .filter(|ip| !ip.is_unspecified())
                .map(IpAddr::V4)
                .collect();
            // Loopback is only useful when the sender runs on this very machine,
            // in which case it also advertises its real interface addresses.
            if ips.iter().any(|ip| !ip.is_loopback()) {
                ips.retain(|ip| !ip.is_loopback());
            }
            if ips.is_empty() || port == 0 || !self.seen.insert(svc.get_fullname().to_string()) {
                continue;
            }
            ips.sort();
            tracing::debug!(service = svc.get_fullname(), ?ips, port, "LAN sender found");
            return Some(
                ips.into_iter()
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect(),
            );
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}
