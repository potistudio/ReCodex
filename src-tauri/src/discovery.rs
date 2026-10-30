//! mDNS advertises reachability only; it never establishes trust or supplies credentials.
use mdns_sd::{
    DaemonEvent, IfKind, IfPredicate, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent,
    ServiceInfo,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV6},
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

const SERVICE: &str = "_recodex._tcp.local.";
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredHost {
    pub id: String,
    pub name: String,
    pub addresses: Vec<String>,
}
struct Record {
    fullname: String,
    host: DiscoveredHost,
}
pub struct Discovery {
    daemon: Option<ServiceDaemon>,
    own_id: String,
    records: Mutex<HashMap<String, Record>>,
    advertised: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
    pub updates: broadcast::Sender<()>,
}
impl Discovery {
    pub fn start(own_id: String) -> Arc<Self> {
        Self::with_daemon(own_id, ServiceDaemon::new())
    }
    pub(crate) fn with_daemon(own_id: String, daemon: mdns_sd::Result<ServiceDaemon>) -> Arc<Self> {
        let (updates, _) = broadcast::channel(16);
        let (daemon, error) = match daemon {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let discovery = Arc::new(Self {
            daemon,
            own_id,
            records: Mutex::new(HashMap::new()),
            advertised: Mutex::new(None),
            error: Mutex::new(error),
            updates,
        });
        if let Some(daemon) = &discovery.daemon {
            let _ = daemon.set_ip_check_interval(5);
            match daemon.browse(SERVICE) {
                Ok(events) => {
                    let weak = Arc::downgrade(&discovery);
                    tokio::spawn(async move {
                        while let Ok(event) = events.recv_async().await {
                            let Some(discovery) = weak.upgrade() else {
                                break;
                            };
                            discovery.accept(event);
                        }
                    });
                }
                Err(e) => *discovery.error.lock().unwrap() = Some(e.to_string()),
            }
            if let Ok(events) = daemon.monitor() {
                let weak = Arc::downgrade(&discovery);
                tokio::spawn(async move {
                    while let Ok(event) = events.recv_async().await {
                        let Some(discovery) = weak.upgrade() else {
                            break;
                        };
                        match event {
                            DaemonEvent::Error(e) => {
                                *discovery.error.lock().unwrap() = Some(e.to_string());
                                let _ = discovery.updates.send(());
                            }
                            DaemonEvent::Announce(_, _) => {
                                *discovery.error.lock().unwrap() = None;
                                let _ = discovery.updates.send(());
                            }
                            _ => {}
                        }
                    }
                });
            }
        }
        discovery
    }
    fn accept(&self, event: ServiceEvent) {
        let mut records = self.records.lock().unwrap();
        let changed = match event {
            ServiceEvent::ServiceResolved(info) => {
                if let Some(host) = parse(&info, &self.own_id) {
                    let changed = records.get(&host.id).is_none_or(|old| old.host != host);
                    if changed {
                        records.insert(
                            host.id.clone(),
                            Record {
                                fullname: info.fullname,
                                host,
                            },
                        );
                    }
                    changed
                } else {
                    false
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                let before = records.len();
                records.retain(|_, record| record.fullname != fullname);
                records.len() != before
            }
            _ => false,
        };
        drop(records);
        if changed {
            *self.error.lock().unwrap() = None;
            let _ = self.updates.send(());
        }
    }
    pub fn snapshot(&self) -> Vec<DiscoveredHost> {
        let mut hosts: Vec<_> = self
            .records
            .lock()
            .unwrap()
            .values()
            .map(|r| r.host.clone())
            .collect();
        hosts.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        hosts
    }
    pub fn addresses(&self, id: &str) -> Vec<String> {
        self.records
            .lock()
            .unwrap()
            .get(id)
            .map(|r| r.host.addresses.clone())
            .unwrap_or_default()
    }
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
    pub fn advertise(&self, id: &str, name: &str, address: &str) -> Result<(), String> {
        let daemon = self
            .daemon
            .as_ref()
            .ok_or("LAN discovery is unavailable; manual connections still work")?;
        let address: SocketAddr = address.parse().map_err(|_| "Invalid listener address")?;
        let hostname = format!("recodex-{id}.local.");
        let properties = [("id", id), ("name", name), ("version", "1")];
        let mut info = ServiceInfo::new(
            SERVICE,
            id,
            &hostname,
            if address.ip().is_unspecified() {
                String::new()
            } else {
                address.ip().to_string()
            },
            address.port(),
            &properties[..],
        )
        .map_err(|e| e.to_string())?;
        if address.ip().is_unspecified() {
            info = info.enable_addr_auto();
            let ipv4 = address.is_ipv4();
            info.set_interfaces(vec![IfKind::Predicate(IfPredicate::new(
                move |interface| interface.ip().is_ipv4() == ipv4 && !interface.ip().is_loopback(),
            ))]);
        } else {
            info.set_interfaces(vec![IfKind::Addr(address.ip())]);
        }
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).map_err(|e| e.to_string())?;
        *self.advertised.lock().unwrap() = Some(fullname);
        Ok(())
    }
    pub fn unadvertise(&self) {
        if let (Some(daemon), Some(fullname)) =
            (&self.daemon, self.advertised.lock().unwrap().take())
        {
            let _ = daemon.unregister(&fullname);
        }
    }
    pub fn shutdown(&self) {
        self.unadvertise();
        if let Some(daemon) = &self.daemon {
            let _ = daemon.shutdown();
        }
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn parse(info: &ResolvedService, own_id: &str) -> Option<DiscoveredHost> {
    let id = info.get_property_val_str("id")?;
    if id == own_id
        || uuid::Uuid::parse_str(id).is_err()
        || info.get_property_val_str("version") != Some("1")
        || info.port == 0
    {
        return None;
    }
    let name: String = info
        .get_property_val_str("name")
        .unwrap_or("ReCodex PC")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let mut addresses: Vec<_> = info
        .addresses
        .iter()
        .filter(|ip| !ip.to_ip_addr().is_unspecified() && !ip.to_ip_addr().is_multicast())
        .filter_map(|ip| match ip {
            ScopedIp::V4(v4) => Some(SocketAddr::new((*v4.addr()).into(), info.port).to_string()),
            ScopedIp::V6(v6) => Some(
                SocketAddr::V6(SocketAddrV6::new(
                    *v6.addr(),
                    info.port,
                    0,
                    v6.scope_id().index,
                ))
                .to_string(),
            ),
            _ => None,
        })
        .collect();
    addresses.sort_by_key(|address| (address.starts_with('['), address.clone()));
    addresses.dedup();
    addresses.truncate(8);
    if addresses.is_empty() {
        return None;
    }
    Some(DiscoveredHost {
        id: id.into(),
        name,
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn catalog_tracks_address_changes_removal_and_never_trusts_discovery() {
        let own = uuid::Uuid::new_v4().to_string();
        let peer = uuid::Uuid::new_v4().to_string();
        // Exercise the actual mDNS record representation without depending on multicast in CI.
        let discovery =
            Discovery::with_daemon(own.clone(), Err(mdns_sd::Error::Msg("test".into())));
        let info = |id: &str, address: &str, version: &str| {
            ServiceInfo::new(
                SERVICE,
                id,
                &format!("{id}.local."),
                address,
                43177,
                &[("id", id), ("name", "Desktop"), ("version", version)][..],
            )
            .unwrap()
            .as_resolved_service()
        };
        discovery.accept(ServiceEvent::ServiceResolved(Box::new(info(
            &own,
            "192.168.1.1",
            "1",
        ))));
        discovery.accept(ServiceEvent::ServiceResolved(Box::new(info(
            &peer,
            "192.168.1.2",
            "2",
        ))));
        assert!(discovery.snapshot().is_empty());
        discovery.accept(ServiceEvent::ServiceResolved(Box::new(info(
            &peer,
            "192.168.1.2",
            "1",
        ))));
        assert_eq!(discovery.addresses(&peer), vec!["192.168.1.2:43177"]);
        discovery.accept(ServiceEvent::ServiceResolved(Box::new(info(
            &peer,
            "192.168.1.3",
            "1",
        ))));
        assert_eq!(discovery.addresses(&peer), vec!["192.168.1.3:43177"]);
        discovery.accept(ServiceEvent::ServiceRemoved(
            SERVICE.into(),
            format!("{peer}.{SERVICE}"),
        ));
        assert!(discovery.snapshot().is_empty());
    }
    #[tokio::test]
    #[ignore = "Requires local multicast permissions; uses a separate UDP discovery port"]
    async fn live_mdns_discovery_and_removal() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let publisher_id = uuid::Uuid::new_v4().to_string();
        let browser = Discovery::with_daemon(
            uuid::Uuid::new_v4().to_string(),
            ServiceDaemon::new_with_port(port),
        );
        let publisher =
            Discovery::with_daemon(publisher_id.clone(), ServiceDaemon::new_with_port(port));
        let mut updates = browser.updates.subscribe();
        publisher
            .advertise(&publisher_id, "Test PC", "127.0.0.1:43177")
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while browser.addresses(&publisher_id).is_empty() {
                updates.recv().await.unwrap();
            }
        })
        .await
        .expect("mDNS should discover the publisher");
        assert_eq!(browser.snapshot()[0].name, "Test PC");
        publisher.unadvertise();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !browser.addresses(&publisher_id).is_empty() {
                updates.recv().await.unwrap();
            }
        })
        .await
        .expect("mDNS should withdraw a stopped host");
        publisher.shutdown();
        browser.shutdown();
    }
}
