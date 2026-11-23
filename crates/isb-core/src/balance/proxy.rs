//! PROXY protocol v2 headers (haproxy's `proxy-protocol.txt`, section 2.2).
//!
//! A route with the option on writes one before the first byte it relays, so
//! the service behind it learns the client's address instead of seeing every
//! connection come from the bridge gateway. Only the binary v2 form is
//! written: it is a fixed layout, and every server that reads v1 reads v2.

use std::net::{IpAddr, SocketAddr};

/// The 12 bytes every v2 header starts with.
pub const SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";
/// Version 2, command PROXY (the addresses below are the real connection's).
const VERSION_PROXY: u8 = 0x21;
/// AF_INET or AF_INET6, over STREAM.
const TCP4: u8 = 0x11;
const TCP6: u8 = 0x21;

/// The v2 header for a TCP connection from `src` (the client) to `dst` (the
/// address it connected to).
///
/// A dual-stack listener reports an IPv4 client as `::ffff:a.b.c.d`; that is
/// sent as plain IPv4, since the service's allowlists and bans are written in
/// IPv4. When one side is IPv4 and the other IPv6, both go as IPv6 (the IPv4
/// one mapped), because a header carries a single family.
pub fn header(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    let (s, d) = (unmap(src.ip()), unmap(dst.ip()));
    let mut out = Vec::with_capacity(16 + 36);
    out.extend_from_slice(&SIGNATURE);
    out.push(VERSION_PROXY);
    match (s, d) {
        (IpAddr::V4(s), IpAddr::V4(d)) => {
            out.push(TCP4);
            out.extend_from_slice(&12u16.to_be_bytes());
            out.extend_from_slice(&s.octets());
            out.extend_from_slice(&d.octets());
        }
        _ => {
            out.push(TCP6);
            out.extend_from_slice(&36u16.to_be_bytes());
            out.extend_from_slice(&v6(s).octets());
            out.extend_from_slice(&v6(d).octets());
        }
    }
    out.extend_from_slice(&src.port().to_be_bytes());
    out.extend_from_slice(&dst.port().to_be_bytes());
    out
}

fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn v6(ip: IpAddr) -> std::net::Ipv6Addr {
    match ip {
        IpAddr::V4(v) => v.to_ipv6_mapped(),
        IpAddr::V6(v) => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv4() {
        let h = header(addr("203.0.113.9:51234"), addr("85.155.178.126:993"));
        let mut want = SIGNATURE.to_vec();
        want.extend_from_slice(&[0x21, 0x11, 0x00, 0x0c]);
        want.extend_from_slice(&[203, 0, 113, 9, 85, 155, 178, 126]);
        want.extend_from_slice(&[0xc8, 0x22, 0x03, 0xe1]); // 51234, 993
        assert_eq!(h, want);
    }

    #[test]
    fn ipv6() {
        let h = header(addr("[2001:db8::1]:40000"), addr("[2001:db8::2]:465"));
        assert_eq!(&h[..12], &SIGNATURE);
        assert_eq!(&h[12..16], &[0x21, 0x21, 0x00, 0x24]);
        assert_eq!(h.len(), 16 + 36);
        let ip = |b: &[u8]| std::net::Ipv6Addr::from(<[u8; 16]>::try_from(b).unwrap());
        assert_eq!(
            ip(&h[16..32]),
            "2001:db8::1".parse::<std::net::Ipv6Addr>().unwrap()
        );
        assert_eq!(
            ip(&h[32..48]),
            "2001:db8::2".parse::<std::net::Ipv6Addr>().unwrap()
        );
        assert_eq!(&h[48..], &[0x9c, 0x40, 0x01, 0xd1]); // 40000, 465
    }

    #[test]
    fn mapped_ipv4_is_sent_as_ipv4() {
        let h = header(
            addr("[::ffff:198.51.100.4]:1000"),
            addr("[::ffff:10.0.0.1]:993"),
        );
        assert_eq!(h, header(addr("198.51.100.4:1000"), addr("10.0.0.1:993")));
        assert_eq!(h[13], TCP4);
    }

    #[test]
    fn mixed_families_go_as_ipv6() {
        let h = header(addr("198.51.100.4:1000"), addr("[2001:db8::2]:993"));
        assert_eq!(h[13], TCP6);
        assert_eq!(
            &h[16..32],
            &"::ffff:198.51.100.4"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets()
        );
    }
}
