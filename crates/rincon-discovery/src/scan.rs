//! The network side of discovery: multicast probing and bounded description fetching.
//!
//! # Why every interface is probed
//!
//! A developer laptop routinely has five or six IPv4 interfaces: Wi-Fi, Ethernet, a VPN tunnel,
//! a Hyper-V or WSL virtual switch, and a Docker bridge. Sending one multicast probe from the
//! "default" interface picks whichever the routing table prefers, which on such a machine is
//! frequently a virtual switch with no speakers on it. The scan then reports "no devices found"
//! on a network that has four.
//!
//! So Rincon probes from **each** interface and records which local address reached each
//! device. That recorded address is what the stream server later binds to, which is the
//! difference between a working session and a URL the speaker cannot route back to.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdpSocket};
use std::time::Duration;

use rincon_core::device::Device;
use rincon_core::limits;
use rincon_core::net;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{debug, trace, warn};
use url::Url;

use crate::description::{self, DescriptionError};
use crate::ssdp::{self, SsdpError, SsdpResponse};

/// Why a scan could not run. Note that "found nothing" is not an error — it is an outcome the
/// interface has to render, with a different explanation from "the socket failed".
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// No usable IPv4 interface exists.
    #[error("no usable network interface found")]
    NoInterface,

    /// Every interface failed to open a socket.
    #[error("could not open a discovery socket on any interface: {detail}")]
    SocketFailed {
        /// The last underlying error, for the log.
        detail: String,
    },
}

/// A device that answered but whose description could not be fetched or parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedDevice {
    /// Where it answered from.
    pub address: IpAddr,
    /// Its UDN, which the SSDP reply already gave us.
    pub udn: String,
    /// Why the description was unusable, already redacted for display.
    pub reason: String,
}

/// The result of one scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Fully resolved players, sorted by room name for a stable interface.
    pub devices: Vec<Device>,
    /// Devices that answered but could not be described.
    pub unresolved: Vec<UnresolvedDevice>,
    /// Interfaces the probe actually went out on, for the diagnostics bundle.
    pub probed_interfaces: Vec<Ipv4Addr>,
}

impl ScanOutcome {
    /// Whether the scan found nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.unresolved.is_empty()
    }
}

/// Knobs for a scan. Defaults come from [`rincon_core::limits`] so they are auditable in one place.
#[derive(Debug, Clone, Copy)]
pub struct ScanConfig {
    /// How long to listen for replies.
    pub window: Duration,
    /// Maximum devices retained, so a flooding responder cannot exhaust memory.
    pub max_devices: usize,
    /// Maximum concurrent description fetches.
    pub max_concurrent_fetches: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            window: limits::DISCOVERY_WINDOW,
            max_devices: limits::MAX_DEVICES_PER_SCAN,
            max_concurrent_fetches: limits::MAX_CONCURRENT_FETCHES,
        }
    }
}

/// How likely an interface is to be the one a speaker is on.
///
/// The ordering matters twice. During a scan it decides which interface answers first, so the
/// device list settles sooner. More importantly, anything that has to pick *one* interface —
/// the `stream` command, a diagnostic — must not pick a virtual switch, because binding a
/// server there produces a URL nothing on the real network can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InterfaceKind {
    /// A physical adapter: Wi-Fi, Ethernet. Where speakers actually live.
    Physical,
    /// A hypervisor or container switch: WSL, Hyper-V, Docker, VirtualBox, VMware.
    ///
    /// Still probed — nothing stops someone bridging a speaker onto one — but never the
    /// default choice, and never preferred over a physical adapter.
    Virtual,
    /// Loopback. Probed last, and only because the test harness runs the real discovery path
    /// against a fake responder there.
    Loopback,
}

/// An address to probe from, with enough context to rank it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeInterface {
    /// The local address.
    pub address: Ipv4Addr,
    /// The OS's name for the adapter.
    pub name: String,
    /// What kind of adapter it appears to be.
    pub kind: InterfaceKind,
}

/// Substrings that mark an adapter as belonging to a hypervisor or container runtime.
///
/// Matching on the name is a heuristic, and it is the only signal available: a WSL switch on
/// `172.26.80.1` is inside RFC1918 exactly like a home LAN, so the address cannot distinguish
/// them. The cost of a false positive is small — the interface is still probed, just last.
const VIRTUAL_MARKERS: [&str; 8] =
    ["vethernet", "hyper-v", "wsl", "docker", "virtualbox", "vmware", "vmnet", "tailscale"];

/// Classifies an adapter from its name and address.
#[must_use]
pub fn classify_interface(name: &str, address: Ipv4Addr) -> InterfaceKind {
    if address.is_loopback() {
        return InterfaceKind::Loopback;
    }
    let lowered = name.to_ascii_lowercase();
    if VIRTUAL_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return InterfaceKind::Virtual;
    }
    InterfaceKind::Physical
}

/// Lists the interfaces worth probing from, best candidate first.
///
/// Loopback is included deliberately: it is how the test harness runs the real discovery path
/// against a fake responder in CI, with no hardware and no LAN.
#[must_use]
pub fn probe_interfaces_detailed() -> Vec<ProbeInterface> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return vec![ProbeInterface {
            address: Ipv4Addr::LOCALHOST,
            name: "loopback".to_owned(),
            kind: InterfaceKind::Loopback,
        }];
    };

    let mut found: Vec<ProbeInterface> = interfaces
        .into_iter()
        .filter_map(|iface| match iface.addr.ip() {
            IpAddr::V4(v4) if !v4.is_unspecified() => Some(ProbeInterface {
                kind: classify_interface(&iface.name, v4),
                address: v4,
                name: iface.name,
            }),
            _ => None,
        })
        .collect();

    // Physical first, then virtual, then loopback; address order within a kind, so the list is
    // stable across runs and a test can assert on it.
    found.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.address.cmp(&b.address)));
    found.dedup_by(|a, b| a.address == b.address);

    if found.is_empty() {
        found.push(ProbeInterface {
            address: Ipv4Addr::LOCALHOST,
            name: "loopback".to_owned(),
            kind: InterfaceKind::Loopback,
        });
    }
    found
}

/// Lists the IPv4 addresses worth probing from, best candidate first.
#[must_use]
pub fn probe_interfaces() -> Vec<Ipv4Addr> {
    probe_interfaces_detailed().into_iter().map(|iface| iface.address).collect()
}

/// The single best interface to bind a server to when no device has been discovered yet.
///
/// Returns `None` only when the machine has nothing but loopback, which the caller should
/// report rather than paper over.
#[must_use]
pub fn preferred_lan_interface() -> Option<Ipv4Addr> {
    probe_interfaces_detailed()
        .into_iter()
        .find(|iface| iface.kind == InterfaceKind::Physical)
        .map(|iface| iface.address)
}

/// Runs a full discovery scan.
///
/// # Errors
///
/// Returns [`ScanError`] only when no socket could be opened anywhere. An empty result is a
/// successful scan that found nothing, which the caller must distinguish.
///
/// Covers: RQ-DISC-009, RQ-DISC-010, RQ-DISC-011
pub async fn scan(config: ScanConfig) -> Result<ScanOutcome, ScanError> {
    let interfaces = probe_interfaces();
    if interfaces.is_empty() {
        return Err(ScanError::NoInterface);
    }

    let mut responses: BTreeMap<String, (SsdpResponse, Ipv4Addr)> = BTreeMap::new();
    let mut probed = Vec::new();
    let mut last_error = None;

    for local in interfaces {
        match probe_one(local, config).await {
            Ok(found) => {
                probed.push(local);
                for response in found {
                    if responses.len() >= config.max_devices {
                        warn!(cap = config.max_devices, "device cap reached; ignoring the rest");
                        break;
                    }
                    // Deduplicate by UDN: a device answers once per interface it hears us on,
                    // and a multi-homed speaker answers several times on one interface.
                    responses.entry(response.udn().to_owned()).or_insert((response, local));
                }
            }
            Err(err) => {
                trace!(%local, %err, "interface unusable for discovery");
                last_error = Some(err.to_string());
            }
        }
    }

    if probed.is_empty() {
        return Err(ScanError::SocketFailed {
            detail: last_error.unwrap_or_else(|| "unknown".to_owned()),
        });
    }

    let mut outcome = resolve_all(responses, config).await;
    outcome.probed_interfaces = probed;
    outcome.devices.sort_by(|a, b| a.room.as_str().cmp(b.room.as_str()));
    Ok(outcome)
}

/// Sends one probe from `local` and collects replies until the window closes.
async fn probe_one(local: Ipv4Addr, config: ScanConfig) -> std::io::Result<Vec<SsdpResponse>> {
    let socket = bind_probe_socket(local)?;
    let target: SocketAddr = format!("{}:{}", ssdp::MULTICAST_ADDR, ssdp::MULTICAST_PORT)
        .parse()
        .map_err(|_| std::io::Error::other("invalid multicast address"))?;

    let request = ssdp::msearch(ssdp::ZONE_PLAYER_ST, 1);
    socket.send_to(&request, target).await?;

    let mut found = Vec::new();
    let mut buffer = vec![0_u8; limits::MAX_SSDP_DATAGRAM];
    let deadline = tokio::time::Instant::now() + config.window;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await {
            Ok(Ok((len, from))) => {
                let Some(datagram) = buffer.get(..len) else { continue };
                match ssdp::parse_response(datagram) {
                    Ok(response) if response.is_zone_player() => {
                        trace!(%from, location = %response.location, "ssdp reply");
                        found.push(response);
                    }
                    Ok(_) => trace!(%from, "ignoring a non-player UPnP device"),
                    Err(SsdpError::MissingHeader(_) | SsdpError::NotHttpShaped) => {}
                    Err(err) => debug!(%from, %err, "discarding malformed ssdp reply"),
                }
            }
            // A receive error on one interface must not abort the whole scan.
            Ok(Err(err)) => {
                debug!(%local, %err, "recv failed");
                break;
            }
            Err(_elapsed) => break,
        }
    }

    Ok(found)
}

/// Binds a UDP socket on a specific interface, configured for multicast egress.
fn bind_probe_socket(local: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // Ephemeral port on this specific interface: that is what pins the probe's egress path.
    socket.bind(&SocketAddrV4::new(local, 0).into())?;
    socket.set_multicast_if_v4(&local)?;
    // One hop: speakers are on the local segment, and a larger TTL leaks probes to other
    // segments on a routed home network for no benefit.
    socket.set_multicast_ttl_v4(1)?;
    socket.set_nonblocking(true)?;

    let std_socket: StdUdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

/// Fetches and parses descriptions for every deduplicated response, with bounded concurrency.
async fn resolve_all(
    responses: BTreeMap<String, (SsdpResponse, Ipv4Addr)>,
    config: ScanConfig,
) -> ScanOutcome {
    let client = match crate::fetch::client() {
        Ok(client) => client,
        Err(err) => {
            warn!(%err, "cannot build an HTTP client; reporting every device as unresolved");
            return ScanOutcome {
                devices: Vec::new(),
                unresolved: responses
                    .into_values()
                    .filter_map(|(response, _)| {
                        Url::parse(&response.location).ok().map(|url| UnresolvedDevice {
                            address: url
                                .host_str()
                                .and_then(|h| h.parse().ok())
                                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                            udn: response.udn().to_owned(),
                            reason: err.to_string(),
                        })
                    })
                    .collect(),
                probed_interfaces: Vec::new(),
            };
        }
    };

    let mut outcome = ScanOutcome::default();
    // A simple bounded fan-out: chunking keeps concurrency capped without a semaphore, and the
    // chunk size is small enough that stragglers cost at most one description timeout.
    let entries: Vec<(SsdpResponse, Ipv4Addr)> = responses.into_values().collect();
    for chunk in entries.chunks(config.max_concurrent_fetches.max(1)) {
        let futures = chunk.iter().map(|(response, local)| resolve_one(&client, response, *local));
        for result in futures::future::join_all(futures).await {
            match result {
                Ok(device) => outcome.devices.push(device),
                Err(unresolved) => outcome.unresolved.push(unresolved),
            }
        }
    }
    outcome
}

/// Validates a location, fetches its description, and builds a [`Device`].
async fn resolve_one(
    client: &reqwest::Client,
    response: &SsdpResponse,
    reached_via: Ipv4Addr,
) -> Result<Device, UnresolvedDevice> {
    let fallback = UnresolvedDevice {
        address: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        udn: response.udn().to_owned(),
        reason: String::new(),
    };

    let url = Url::parse(&response.location).map_err(|source| UnresolvedDevice {
        reason: format!("unparseable LOCATION: {source}"),
        ..fallback.clone()
    })?;

    // The SSRF guard. Nothing is dialled before this returns Ok.
    let address = net::validate_device_url(&url)
        .map_err(|source| UnresolvedDevice { reason: source.to_string(), ..fallback.clone() })?;

    let body = crate::fetch::get_bounded(client, url.clone(), limits::Budget::DESCRIPTION)
        .await
        .map_err(|source| UnresolvedDevice {
            address,
            udn: response.udn().to_owned(),
            reason: source.to_string(),
        })?;

    let described = description::parse(&body).map_err(|source: DescriptionError| {
        UnresolvedDevice { address, udn: response.udn().to_owned(), reason: source.to_string() }
    })?;

    Ok(Device {
        id: described.udn,
        room: described.room,
        model: described.model,
        address,
        port: url.port().unwrap_or(1400),
        reached_via: Some(IpAddr::V4(reached_via)),
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use std::time::Instant;

    use super::*;

    #[test]
    fn probing_always_yields_at_least_loopback() {
        // A machine with every interface down must still produce a usable probe list, or the
        // CI harness -- which discovers a fake responder on loopback -- cannot run at all.
        let interfaces = probe_interfaces();
        assert!(!interfaces.is_empty());
    }

    /// Covers: RQ-DISC-010
    #[test]
    fn a_virtual_switch_never_outranks_a_physical_adapter() {
        // The bug this pins down was found by running `rincon stream` on a laptop with WSL
        // installed: the old code sorted by address and took the first non-loopback, which is
        // `172.26.80.1` -- the WSL switch -- ahead of the Wi-Fi on `192.168.0.153`. The server
        // then bound to a network with nothing on it, and produced a URL no speaker could
        // reach. It presents as a firewall problem and is miserable to diagnose.
        assert_eq!(
            classify_interface(
                "vEthernet (WSL (Hyper-V firewall))",
                "172.26.80.1".parse().unwrap()
            ),
            InterfaceKind::Virtual
        );
        assert_eq!(
            classify_interface("Wi-Fi", "192.168.0.153".parse().unwrap()),
            InterfaceKind::Physical
        );
        assert_eq!(classify_interface("lo", Ipv4Addr::LOCALHOST), InterfaceKind::Loopback);

        // The ordering is what actually protects the caller.
        assert!(InterfaceKind::Physical < InterfaceKind::Virtual);
        assert!(InterfaceKind::Virtual < InterfaceKind::Loopback);
    }

    #[test]
    fn every_hypervisor_we_know_about_is_recognised() {
        for name in [
            "vEthernet (Default Switch)",
            "Hyper-V Virtual Ethernet Adapter",
            "docker0",
            "VirtualBox Host-Only Network",
            "VMware Network Adapter VMnet8",
            "vmnet1",
            "Tailscale",
        ] {
            assert_eq!(
                classify_interface(name, "10.0.0.1".parse().unwrap()),
                InterfaceKind::Virtual,
                "`{name}` should be recognised as virtual"
            );
        }
        // And an ordinary adapter is not caught by the heuristic.
        for name in ["Wi-Fi", "Ethernet", "wlan0", "eth0", "en0"] {
            assert_eq!(
                classify_interface(name, "192.168.1.5".parse().unwrap()),
                InterfaceKind::Physical,
                "`{name}` should be physical"
            );
        }
    }

    #[test]
    fn the_preferred_interface_is_never_loopback_or_virtual() {
        // `None` is a legitimate answer on a machine with nothing but loopback; what must
        // never happen is returning an address a speaker cannot route back to.
        if let Some(address) = preferred_lan_interface() {
            assert!(!address.is_loopback(), "picked loopback as the LAN interface");
            let detailed = probe_interfaces_detailed();
            let picked = detailed
                .iter()
                .find(|iface| iface.address == address)
                .expect("the preferred address must come from the probe list");
            assert_eq!(picked.kind, InterfaceKind::Physical);
        }
    }

    #[test]
    fn the_default_config_matches_the_audited_limits() {
        let config = ScanConfig::default();
        assert_eq!(config.window, limits::DISCOVERY_WINDOW);
        assert_eq!(config.max_devices, limits::MAX_DEVICES_PER_SCAN);
        assert_eq!(config.max_concurrent_fetches, limits::MAX_CONCURRENT_FETCHES);
    }

    /// Covers: RQ-DISC-011
    #[tokio::test]
    async fn rq_disc_011_respects_timeout() {
        // No responder exists, so this exercises the worst case: every interface waits out
        // the full window. It must still return inside window + slack.
        let config = ScanConfig { window: Duration::from_millis(300), ..Default::default() };
        let started = Instant::now();
        let outcome =
            scan(config).await.expect("a scan with no devices is a success, not an error");
        let elapsed = started.elapsed();

        assert!(outcome.is_empty() || !outcome.devices.is_empty());
        let interfaces = probe_interfaces().len() as u32;
        let ceiling = config.window * interfaces + limits::DISCOVERY_SLACK * 2;
        assert!(elapsed <= ceiling, "scan took {elapsed:?}, over the {ceiling:?} ceiling");
    }

    /// Covers: RQ-DISC-009
    #[test]
    fn rq_disc_009_deduplicates_by_udn() {
        // The deduplication key is the UDN, so the same speaker answering on three interfaces
        // appears once. Exercised here at the map level, which is where the rule lives.
        let mut responses: BTreeMap<String, (SsdpResponse, Ipv4Addr)> = BTreeMap::new();
        let make = |location: &str| SsdpResponse {
            location: location.to_owned(),
            search_target: ssdp::ZONE_PLAYER_ST.to_owned(),
            usn: "uuid:RINCON_SAME::urn:schemas-upnp-org:device:ZonePlayer:1".to_owned(),
            server: None,
        };

        for (index, location) in
            ["http://192.168.1.45:1400/x", "http://10.0.0.9:1400/x"].into_iter().enumerate()
        {
            let response = make(location);
            let iface = Ipv4Addr::new(192, 168, 1, 20 + index as u8);
            responses.entry(response.udn().to_owned()).or_insert((response, iface));
        }

        assert_eq!(responses.len(), 1, "one speaker must not appear twice");
        // First answer wins, so the interface recorded is the one that reached it first.
        assert_eq!(responses.values().next().unwrap().0.location, "http://192.168.1.45:1400/x");
    }

    #[test]
    fn an_empty_scan_is_distinguishable_from_a_failure() {
        // The interface renders very different things for these two, so the type must keep
        // them apart rather than collapsing both into an empty vector.
        let empty = ScanOutcome::default();
        assert!(empty.is_empty());
        assert!(ScanError::NoInterface.to_string().contains("interface"));
    }
}
