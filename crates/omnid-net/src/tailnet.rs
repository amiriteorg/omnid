//! Whether a router or node elsewhere can be asked at all. One on the owner's
//! Tailscale network has an address in 100.64.0.0/10, the shared address
//! space Tailscale uses, but carrier-grade NAT uses the same range: with
//! Tailscale down on mobile data, that address can be another subscriber's
//! device, and the plain HTTP sent to a router (eepsite names, Hyphanet keys,
//! whole requests) would go to it. So such a router is asked only while this
//! machine is on a tailnet itself, which is an address in the range on a
//! tunnel interface, not on Wi-Fi or a cellular link that CGNAT numbers the
//! same way.

use std::net::Ipv4Addr;

use url::Url;

/// Whether `ip` is in 100.64.0.0/10.
fn shared_space(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

/// Whether a router at `url` is reached over a tailnet: a literal address in
/// 100.64.0.0/10. A name is resolved where it is meant to be (MagicDNS names
/// only resolve on the tailnet), so only literals are checked.
pub(crate) fn needs_tailnet(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => shared_space(ip),
        // The host of a scheme the URL standard does not know (a SAM bridge's
        // sam://100.74.26.84:7656) stays text, address or not.
        Some(url::Host::Domain(host)) => host.parse().is_ok_and(shared_space),
        _ => false,
    }
}

/// Whether an interface's address puts this machine on a tailnet: in the
/// range, on a tunnel (Tailscale's `tailscale0` on Linux, a `utun` on macOS,
/// the VPN's `tun` on Android).
fn tailnet_interface(name: &str, ip: Ipv4Addr) -> bool {
    shared_space(ip) && ["tailscale", "utun", "tun", "wg"].iter().any(|prefix| name.starts_with(prefix))
}

/// Whether this machine is on a tailnet now.
#[cfg(unix)]
pub(crate) fn on_tailnet() -> bool {
    interfaces().into_iter().any(|(name, ip)| tailnet_interface(&name, ip))
}

/// Elsewhere there is no way to tell here; the router is asked as before.
#[cfg(not(unix))]
pub(crate) fn on_tailnet() -> bool {
    true
}

/// Every interface's IPv4 addresses, by interface name.
#[cfg(unix)]
fn interfaces() -> Vec<(String, Ipv4Addr)> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list it allocated, which
    // is read here only while it lives and handed back to freeifaddrs once.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return found;
        }
        let mut entry = list;
        while let Some(ifa) = entry.as_ref() {
            if !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let addr = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                found.push((name, ip));
            }
            entry = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_literal_shared_space_addresses_need_a_tailnet() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(needs_tailnet(&url("http://100.74.26.84:4444/")));
        assert!(needs_tailnet(&url("http://100.64.0.1/")) && needs_tailnet(&url("http://100.127.255.254/")));
        assert!(!needs_tailnet(&url("http://100.63.255.255/")) && !needs_tailnet(&url("http://100.128.0.1/")));
        assert!(!needs_tailnet(&url("http://127.0.0.1:4444/")) && !needs_tailnet(&url("http://192.168.1.5:8888/")));
        assert!(!needs_tailnet(&url("http://galahads-mac-mini:8888/")), "a name resolves where it is meant to");
        assert!(needs_tailnet(&url("sam://100.74.26.84:7656")), "a SAM bridge's host is text");
        assert!(!needs_tailnet(&url("sam://127.0.0.1:7656")) && !needs_tailnet(&url("sam://galahads-mac-mini:7656")));
    }

    #[test]
    fn a_carrier_address_is_not_a_tailnet() {
        let ip: Ipv4Addr = "100.74.26.84".parse().unwrap();
        assert!(tailnet_interface("tun0", ip) && tailnet_interface("utun4", ip) && tailnet_interface("tailscale0", ip));
        assert!(!tailnet_interface("rmnet_data0", ip), "CGNAT numbers cellular links the same way");
        assert!(!tailnet_interface("wlan0", ip) && !tailnet_interface("en0", ip));
        assert!(!tailnet_interface("tun0", "10.8.0.2".parse().unwrap()), "another VPN");
    }

    #[cfg(unix)]
    #[test]
    fn lists_this_machines_interfaces() {
        assert!(interfaces().iter().any(|(_, ip)| ip.is_loopback()), "loopback is always there");
    }
}
