//! One trusted network (an address or a CIDR range) and its parser.

use std::net::IpAddr;

/// An address with a prefix length, e.g. `10.0.0.0/8` or `fd00::/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    /// Parses `10.0.0.0/8`, `fd00::/8` or a single address (full prefix). `None` if invalid.
    pub(crate) fn parse(entry: &str) -> Option<Self> {
        let (addr, prefix) = match entry.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (entry, None),
        };
        let addr = canonical(addr.parse().ok()?);
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                p.parse::<u8>().ok().filter(|p| *p <= max)?
            }
            Some(_) => return None,
            None => max,
        };
        Some(Self { addr, prefix })
    }

    /// Whether `ip` lies in this network (IPv4 and IPv6 never match each other).
    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, canonical(ip)) {
            (IpAddr::V4(n), IpAddr::V4(i)) => prefix_eq(&n.octets(), &i.octets(), self.prefix),
            (IpAddr::V6(n), IpAddr::V6(i)) => prefix_eq(&n.octets(), &i.octets(), self.prefix),
            _ => false,
        }
    }
}

/// `ip` with an IPv4-mapped IPv6 address (`::ffff:10.0.0.1`, as a dual-stack listener reports IPv4
/// peers) turned into plain IPv4, so it matches IPv4 networks and logs like one.
pub(crate) fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

/// Whether the first `bits` bits of `a` and `b` are equal.
fn prefix_eq(a: &[u8], b: &[u8], bits: u8) -> bool {
    let bits = usize::from(bits);
    a.iter().zip(b).enumerate().all(|(i, (x, y))| {
        let rest = bits.saturating_sub(i * 8).min(8);
        let mask = if rest == 0 { 0 } else { 0xffu8 << (8 - rest) };
        x & mask == y & mask
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn networks() {
        let n = Net::parse("10.0.0.0/8").unwrap();
        assert!(n.contains(ip("10.255.0.1")) && !n.contains(ip("11.0.0.1")));
        assert!(n.contains(ip("::ffff:10.1.2.3")), "IPv4-mapped peer");
        let one = Net::parse("192.168.1.1").unwrap();
        assert!(one.contains(ip("192.168.1.1")) && !one.contains(ip("192.168.1.2")));
        let v6 = Net::parse("fd00::/8").unwrap();
        assert!(v6.contains(ip("fd12::1")) && !v6.contains(ip("fe80::1")));
        let odd = Net::parse("172.16.0.0/12").unwrap();
        assert!(odd.contains(ip("172.31.255.255")) && !odd.contains(ip("172.32.0.0")));
        assert!(Net::parse("0.0.0.0/0").unwrap().contains(ip("203.0.113.9")));
        assert!(!Net::parse("0.0.0.0/0").unwrap().contains(ip("2001:db8::1")));
    }

    #[test]
    fn invalid_entries() {
        for bad in [
            "10.0.0.0/33",
            "fd00::/129",
            "proxy",
            "10.0.0.0/",
            "10.0.0.0/+8",
            "10.0.0.0/8/8",
            "",
        ] {
            assert_eq!(Net::parse(bad), None, "{bad}");
        }
    }
}
