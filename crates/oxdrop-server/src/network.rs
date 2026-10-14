use std::net::IpAddr;

/// Picks the first private IPv4 address on an up, non-loopback interface.
///
/// Deliberately naive: proper ranking (default route first, VPN and virtual
/// adapters last) and the interface dropdown arrive with issue #3.
pub(crate) fn pick_address() -> Option<IpAddr> {
    let interfaces = if_addrs::get_if_addrs().ok()?;
    interfaces
        .iter()
        .filter(|interface| interface.is_oper_up() && !interface.is_loopback())
        .find_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if ip.is_private() => Some(IpAddr::V4(ip)),
            _ => None,
        })
}
