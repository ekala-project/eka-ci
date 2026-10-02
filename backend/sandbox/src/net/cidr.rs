use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use anyhow::{Context, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv4Cidr {
    addr: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Cidr {
    pub fn new(addr: Ipv4Addr, prefix: u8) -> anyhow::Result<Self> {
        if prefix > 32 {
            bail!("prefix length {prefix} is larger than 32");
        }
        if u32::from(addr) & !mask(prefix) != 0 {
            bail!("{addr}/{prefix} has host bits set");
        }
        Ok(Self { addr, prefix })
    }

    pub fn host(addr: Ipv4Addr) -> Self {
        Self { addr, prefix: 32 }
    }

    pub fn addr(&self) -> Ipv4Addr {
        self.addr
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }
}

fn mask(prefix: u8) -> u32 {
    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
}

impl FromStr for Ipv4Cidr {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (
                a,
                p.parse().with_context(|| format!("bad prefix in {s:?}"))?,
            ),
            None => (s, 32),
        };
        let addr = addr
            .parse()
            .with_context(|| format!("{s:?} is not an IPv4 address or CIDR"))?;
        Self::new(addr, prefix).with_context(|| format!("invalid CIDR {s:?}"))
    }
}

impl fmt::Display for Ipv4Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays() {
        let c: Ipv4Cidr = "10.0.0.0/8".parse().unwrap();
        assert_eq!((c.addr(), c.prefix()), (Ipv4Addr::new(10, 0, 0, 0), 8));
        assert_eq!(c.to_string(), "10.0.0.0/8");
        assert_eq!(
            "1.2.3.4".parse::<Ipv4Cidr>().unwrap().to_string(),
            "1.2.3.4/32"
        );
        assert_eq!("0.0.0.0/0".parse::<Ipv4Cidr>().unwrap().prefix(), 0);
    }

    #[test]
    fn rejects_invalid() {
        for bad in [
            "10.0.0.1/8",
            "10.0.0.0/33",
            "::1/128",
            "10.0.0.0/x",
            "",
            "host",
        ] {
            assert!(bad.parse::<Ipv4Cidr>().is_err(), "{bad} accepted");
        }
    }
}
