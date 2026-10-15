//! Choosing the network interface the Receiver can most likely reach.

use std::net::{IpAddr, Ipv4Addr};

/// A network interface as the OS reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterface {
    /// e.g. `en0` on macOS, `Wi-Fi` or `vEthernet (WSL)` on Windows.
    pub name: String,
    pub up: bool,
    pub loopback: bool,
    pub addresses: Vec<IpAddr>,
    /// Whether the OS sends internet-bound traffic out through this interface.
    pub default_route: bool,
}

/// An interface address a Transfer Link can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub interface: String,
    pub ip: Ipv4Addr,
}

/// The addresses a Transfer Link could use, most likely to reach the Receiver first.
pub fn rank_interfaces(interfaces: &[NetworkInterface]) -> Vec<Candidate> {
    let mut ranked: Vec<(&NetworkInterface, Ipv4Addr)> = interfaces
        .iter()
        .filter(|interface| interface.up && !interface.loopback)
        .flat_map(|interface| {
            interface
                .addresses
                .iter()
                .filter_map(move |address| match address {
                    IpAddr::V4(ip) if ip.is_private() => Some((interface, *ip)),
                    _ => None,
                })
        })
        .collect();
    // Virtual adapters go last even when they hold the default route (a full-tunnel
    // VPN): the phone can't reach them. Stable, so ties keep the OS's order.
    ranked.sort_by_key(|(interface, _)| (is_virtual(&interface.name), !interface.default_route));
    ranked
        .into_iter()
        .map(|(interface, ip)| Candidate {
            interface: interface.name.clone(),
            ip,
        })
        .collect()
}

/// Name prefixes of VPN, tunnel and virtual-machine adapters, lowercase.
const VIRTUAL_PREFIXES: [&str; 8] = [
    "utun",
    "tun",
    "wg",
    "docker",
    "vethernet",
    "vmnet",
    "vboxnet",
    "bridge",
];

/// Words that mark a VPN or virtual adapter wherever they appear in its name, lowercase.
const VIRTUAL_WORDS: [&str; 3] = ["tailscale", "zerotier", "virtualbox"];

/// Whether `name` looks like a VPN, tunnel or virtual adapter, which a phone on
/// the same Wi-Fi can't reach.
fn is_virtual(name: &str) -> bool {
    let name = name.to_lowercase();
    VIRTUAL_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || VIRTUAL_WORDS.iter().any(|word| name.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(name: &str, addresses: &[&str]) -> NetworkInterface {
        NetworkInterface {
            name: name.to_owned(),
            up: true,
            loopback: false,
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
            default_route: false,
        }
    }

    fn default_route(interface: NetworkInterface) -> NetworkInterface {
        NetworkInterface {
            default_route: true,
            ..interface
        }
    }

    fn loopback() -> NetworkInterface {
        NetworkInterface {
            loopback: true,
            ..up("lo0", &["127.0.0.1", "::1"])
        }
    }

    fn ranked(interfaces: &[NetworkInterface]) -> Vec<(String, String)> {
        rank_interfaces(interfaces)
            .into_iter()
            .map(|c| (c.interface, c.ip.to_string()))
            .collect()
    }

    fn expect(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, ip)| (name.to_string(), ip.to_string()))
            .collect()
    }

    #[test]
    fn macbook_with_wifi_and_vpn_tunnels_prefers_wifi() {
        let machine = [
            loopback(),
            up("utun0", &["fe80::1"]),
            up("utun4", &["10.8.0.2"]),
            default_route(up("en0", &["fe80::2", "192.168.1.20"])),
            up("awdl0", &["fe80::3"]),
        ];
        assert_eq!(
            ranked(&machine),
            expect(&[("en0", "192.168.1.20"), ("utun4", "10.8.0.2")])
        );
    }

    #[test]
    fn windows_pc_puts_real_adapters_before_wsl_and_docker() {
        let machine = [
            up("vEthernet (WSL)", &["172.29.16.1"]),
            up("vEthernet (Default Switch)", &["172.20.80.1"]),
            up("docker0", &["172.17.0.1"]),
            up("Wi-Fi", &["192.168.1.34"]),
            default_route(up("Ethernet", &["192.168.1.33"])),
            loopback(),
        ];
        assert_eq!(
            ranked(&machine),
            expect(&[
                ("Ethernet", "192.168.1.33"),
                ("Wi-Fi", "192.168.1.34"),
                ("vEthernet (WSL)", "172.29.16.1"),
                ("vEthernet (Default Switch)", "172.20.80.1"),
                ("docker0", "172.17.0.1"),
            ])
        );
    }

    #[test]
    fn a_full_tunnel_vpn_holding_the_default_route_still_ranks_last() {
        let machine = [
            loopback(),
            default_route(up("utun3", &["10.200.0.7"])),
            up("en0", &["192.168.0.12"]),
        ];
        assert_eq!(
            ranked(&machine),
            expect(&[("en0", "192.168.0.12"), ("utun3", "10.200.0.7")])
        );
    }

    #[test]
    fn office_network_on_10_x_with_a_disconnected_adapter() {
        let unplugged = NetworkInterface {
            up: false,
            ..up("en1", &["192.168.50.2"])
        };
        let machine = [
            loopback(),
            unplugged,
            default_route(up("en0", &["10.12.4.88"])),
        ];
        assert_eq!(ranked(&machine), expect(&[("en0", "10.12.4.88")]));
    }

    #[test]
    fn loopback_only_has_no_candidates() {
        assert_eq!(ranked(&[loopback()]), expect(&[]));
    }

    #[test]
    fn tailscale_only_has_no_candidates() {
        // Tailscale uses carrier-grade NAT space (100.64/10), never a private range.
        let machine = [
            loopback(),
            default_route(up("Tailscale", &["100.101.102.103", "fd7a:115c:a1e0::1"])),
        ];
        assert_eq!(ranked(&machine), expect(&[]));
    }

    #[test]
    fn ipv6_and_public_addresses_are_never_candidates() {
        let machine = [default_route(up(
            "en0",
            &["2001:db8::5", "fd00::5", "203.0.113.9"],
        ))];
        assert_eq!(ranked(&machine), expect(&[]));
    }
}
