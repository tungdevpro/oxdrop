use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use oxdrop_core::{Candidate, NetworkInterface, rank_interfaces};

/// This computer's addresses a Transfer Link could use, most likely to reach the
/// Receiver first. Empty when there's no usable local network connection.
pub fn interface_candidates() -> Vec<Candidate> {
    let Ok(addresses) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let default_route = default_route_ip();
    let mut interfaces: Vec<NetworkInterface> = Vec::new();
    // The OS lists one entry per address; gather them per interface, keeping its order.
    for address in addresses {
        let ip = address.ip();
        let index = match interfaces.iter().position(|i| i.name == address.name) {
            Some(index) => index,
            None => {
                interfaces.push(NetworkInterface {
                    name: address.name.clone(),
                    up: address.is_oper_up(),
                    loopback: address.is_loopback(),
                    addresses: Vec::new(),
                    default_route: false,
                });
                interfaces.len() - 1
            }
        };
        let interface = &mut interfaces[index];
        interface.default_route |= Some(ip) == default_route;
        interface.addresses.push(ip);
    }
    rank_interfaces(&interfaces)
}

/// The local IPv4 address the OS would send internet traffic from. Connecting a UDP
/// socket only picks a route; no packet is sent.
fn default_route_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket
        .connect(SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), 53)))
        .ok()?;
    Some(socket.local_addr().ok()?.ip())
}
