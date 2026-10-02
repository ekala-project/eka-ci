pub(crate) mod attach;
mod cidr;
pub(crate) mod host;
mod netlink;
pub(crate) mod setup;

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::path::Path;

pub use cidr::Ipv4Cidr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Network {
    #[default]
    None,
    Filtered,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub allow: Vec<Ipv4Cidr>,
    pub deny: Vec<Ipv4Cidr>,
}

pub(crate) const IFNAME: &str = "ekaci0";
pub(crate) const GUEST_ADDR: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
pub(crate) const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub(crate) const DNS_ADDR: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);
pub(crate) const RESOLV_FD: i32 = 3;

pub const DEFAULT_DENY: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Unreachable(Ipv4Cidr),
    ViaGateway(Ipv4Cidr),
}

pub(crate) fn route_plan(policy: &NetworkPolicy, host_addrs: &[Ipv4Addr]) -> Vec<Route> {
    let defaults = DEFAULT_DENY
        .iter()
        .map(|&(a, p)| Ipv4Cidr::new(a, p).expect("DEFAULT_DENY entries are valid"));
    let hosts = host_addrs.iter().copied().map(Ipv4Cidr::host);
    let mut plan: Vec<Route> = defaults
        .chain(hosts)
        .chain(policy.deny.iter().copied())
        .map(Route::Unreachable)
        .collect();
    let default = Ipv4Cidr::new(Ipv4Addr::UNSPECIFIED, 0).expect("0.0.0.0/0 is valid");
    plan.push(Route::ViaGateway(default));
    plan.push(Route::ViaGateway(Ipv4Cidr::host(DNS_ADDR)));
    // Each route replaces an earlier one for the same prefix: allow wins ties.
    plan.extend(policy.allow.iter().copied().map(Route::ViaGateway));
    plan
}

pub(crate) fn pasta_args() -> Vec<OsString> {
    let addr = GUEST_ADDR.to_string();
    let gw = GATEWAY.to_string();
    let dns = DNS_ADDR.to_string();
    [
        "--config-net",
        "--quiet",
        "--foreground",
        "--ipv4-only",
        "--ns-ifname",
        IFNAME,
        "--address",
        &addr,
        "--netmask",
        "24",
        "--gateway",
        &gw,
        "--no-map-gw",
        "--dns-forward",
        &dns,
        "--no-dhcp",
        "--tcp-ports",
        "none",
        "--udp-ports",
        "none",
        "--tcp-ns",
        "none",
        "--udp-ns",
        "none",
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

pub(crate) fn attach_args(pasta: &Path, plan: &[Route]) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec![attach::FLAG.into(), "--pasta".into(), pasta.into()];
    out.extend(setup::route_args(plan));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(s: &str) -> Ipv4Cidr {
        s.parse().unwrap()
    }

    #[test]
    fn plan_denies_defaults_hosts_and_extra_then_allows() {
        let policy = NetworkPolicy {
            allow: vec![cidr("10.1.0.0/16")],
            deny: vec![cidr("8.8.8.8/32")],
        };
        let plan = route_plan(&policy, &[Ipv4Addr::new(203, 0, 113, 7)]);
        let pos = |r: Route| plan.iter().position(|x| *x == r).unwrap();
        let rfc1918 = pos(Route::Unreachable(cidr("10.0.0.0/8")));
        let host = pos(Route::Unreachable(cidr("203.0.113.7/32")));
        let extra = pos(Route::Unreachable(cidr("8.8.8.8/32")));
        let default = pos(Route::ViaGateway(cidr("0.0.0.0/0")));
        let dns = pos(Route::ViaGateway(cidr("192.0.2.53/32")));
        let allow = pos(Route::ViaGateway(cidr("10.1.0.0/16")));
        assert!(rfc1918 < host && host < extra && extra < default);
        assert!(default < dns && dns < allow);
        assert!(plan.contains(&Route::Unreachable(cidr("169.254.0.0/16"))));
        assert!(plan.contains(&Route::Unreachable(cidr("192.0.2.0/24"))));
    }

    #[test]
    fn pasta_never_forwards_host_loopback_or_ipv6() {
        let s: Vec<String> = pasta_args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let s = s.join(" ");
        for needle in [
            "--ipv4-only",
            "--no-map-gw",
            "--tcp-ns none --udp-ns none",
            "--tcp-ports none --udp-ports none",
            "--dns-forward 192.0.2.53",
        ] {
            assert!(s.contains(needle), "missing `{needle}` in `{s}`");
        }
    }

    #[test]
    fn attach_args_carry_pasta_and_routes() {
        let plan = vec![Route::Unreachable(cidr("10.0.0.0/8"))];
        let mut args = attach_args(Path::new("/p/pasta"), &plan);
        assert_eq!(args.remove(0), attach::FLAG);
        let parsed = setup::parse_args(args).unwrap();
        assert_eq!(parsed.pasta.as_deref(), Some(Path::new("/p/pasta")));
        assert_eq!(parsed.routes, plan);
    }
}
