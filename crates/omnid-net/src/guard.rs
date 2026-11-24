//! Keeps clearnet fetches on the public internet. Without this, any URL
//! submitted to the daemon could reach the operator's router admin page or a
//! cloud metadata endpoint.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// Whether an address is reachable on the public internet. Loopback,
/// private, link-local, CGNAT, documentation, benchmarking, multicast and
/// reserved ranges are not, and in IPv6 nothing outside global unicast
/// (2000::/3), Yggdrasil's 200::/7 among it.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let segments = ip.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        // NAT64: the low 32 bits are the IPv4 destination.
        let [.., hi, lo] = segments;
        return is_public_v4(Ipv4Addr::from(((hi as u32) << 16) | lo as u32));
    }
    if segments[0] == 0x2002 {
        // 6to4: the next 32 bits are the IPv4 address it reaches.
        return is_public_v4(Ipv4Addr::from(((segments[1] as u32) << 16) | segments[2] as u32));
    }
    // The public internet is global unicast, 2000::/3. Outside it are not
    // only the reserved and local ranges but Yggdrasil's (200::/7), a network
    // of its own that a machine running it reaches as if it were the
    // internet.
    let global_unicast = segments[0] & 0xe000 == 0x2000;
    let documentation = (segments[0] == 0x2001 && segments[1] == 0x0db8) || segments[0] & 0xfff0 == 0x3ff0;
    global_unicast && !documentation
}

/// IP-literal hosts never reach the resolver, so they are checked here.
pub(crate) fn host_is_public(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => is_public_v4(ip),
        Some(Host::Ipv6(ip)) => is_public_v6(ip),
        _ => true,
    }
}

/// Resolves with the system resolver and drops non-public addresses. The
/// connection uses the filtered addresses, so DNS rebinding cannot slip a
/// private address in after the check.
pub(crate) struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.filter(|addr| is_public_ip(addr.ip())).collect();
            if addrs.is_empty() {
                return Err(format!("{host} resolves only to non-public addresses").into());
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(ip: &str) -> bool {
        is_public_ip(ip.parse().unwrap())
    }

    #[test]
    fn rejects_non_public_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
            // Outside global unicast (2000::/3): Yggdrasil's nodes and
            // subnets (200::/7), the discard prefix, deprecated site-local.
            "200:1111:2222:3333::1",
            "300:1111:2222:3333::1",
            "100::1",
            "fec0::1",
            "5f00::1",
            // Inside it, but documentation (RFC 9637), or 6to4 carrying a
            // private IPv4 address.
            "3fff::1",
            "2002:c0a8:101::1",
        ] {
            assert!(!public(ip), "{ip} should not be public");
        }
    }

    #[test]
    fn accepts_public_ranges() {
        for ip in
            ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111", "::ffff:1.1.1.1", "64:ff9b::101:101", "2002:808:808::1"]
        {
            assert!(public(ip), "{ip} should be public");
        }
    }
}
