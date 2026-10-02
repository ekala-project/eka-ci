#![cfg(target_os = "linux")]

use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

use sandbox::{Network, NetworkPolicy, Sandbox, SandboxExit, SandboxSpec, find_in_path};
use tokio::io::AsyncReadExt;

fn sandbox(policy: NetworkPolicy) -> Sandbox {
    Sandbox::with_paths(find_in_path("bwrap").expect("bwrap"), helper())
        .with_pasta(find_in_path("pasta").expect("pasta"))
        .with_network_policy(policy)
}

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ekaci-sandbox-helper"))
}

async fn run(sb: &Sandbox, network: Network, script: &str) -> String {
    let bash = sandbox::resolve_program("bash").unwrap();
    let spec = SandboxSpec::new(bash)
        .args(["-c", script])
        .env("HELPER", helper().to_string_lossy())
        .ro_path(helper())
        .network(network)
        .timeout(Duration::from_secs(60));
    let mut child = sb.spawn(&spec).await.unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .await
        .unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .await
        .unwrap();
    let exit = child.wait().await.unwrap();
    assert!(matches!(exit, SandboxExit::Exited(_)), "{exit:?} {err}");
    out
}

fn probe(targets: &[String]) -> String {
    targets
        .iter()
        .map(|t| {
            format!(
                "if (exec 3<>/dev/tcp/{ip}/{port}) 2>/dev/null; then echo \"{t} open\"; else echo \
                 \"{t} closed\"; fi;",
                ip = t.split(':').next().unwrap(),
                port = t.split(':').nth(1).unwrap(),
            )
        })
        .collect()
}

fn public_target() -> String {
    use std::net::ToSocketAddrs;
    let addr = ("example.com", 443)
        .to_socket_addrs()
        .unwrap()
        .find(|a| a.is_ipv4())
        .expect("example.com has an IPv4 address");
    addr.to_string()
}

fn host_lan_addr() -> Option<Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("1.1.1.1:53").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(a) if !a.is_loopback() => Some(a),
        _ => None,
    }
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, user namespaces and internet access"]
async fn filtered_network_blocks_private_ranges_and_host() {
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let public = public_target();
    let mut targets = vec![
        public.clone(),
        "10.0.0.1:80".into(),
        "172.16.0.1:80".into(),
        "192.168.1.1:80".into(),
        "169.254.169.254:80".into(),
        "100.64.0.1:80".into(),
        format!("192.0.2.1:{port}"),
        format!("127.0.0.1:{port}"),
    ];
    if let Some(lan) = host_lan_addr() {
        targets.push(format!("{lan}:{port}"));
    }
    let out = run(
        &sandbox(Default::default()),
        Network::Filtered,
        &probe(&targets),
    )
    .await;
    assert!(out.contains(&format!("{public} open")), "{out}");
    for t in &targets[1..] {
        assert!(out.contains(&format!("{t} closed")), "{t} reachable: {out}");
    }
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, user namespaces and internet access"]
async fn filtered_network_resolves_dns_and_routes_are_immutable() {
    let script = "read -r ns < /etc/resolv.conf; echo \"$ns\"; (exec 3<>/dev/tcp/example.com/443) \
                  2>/dev/null && echo dns-ok; \"$HELPER\" --net-setup --via 10.0.0.0/8 -- /none \
                  2>&1; echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null && echo \
                  sysctl-writable; while read -r k v; do [ \"$k\" = CapEff: ] && echo \
                  \"capeff=$v\"; done < /proc/self/status";
    let out = run(&sandbox(Default::default()), Network::Filtered, script).await;
    assert!(out.contains("nameserver 192.0.2.53"), "{out}");
    assert!(out.contains("dns-ok"), "{out}");
    assert!(!out.contains("sysctl-writable"), "{out}");
    assert!(out.contains("Operation not permitted"), "{out}");
    assert!(out.contains("capeff=0000000000000000"), "{out}");
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, user namespaces and internet access"]
async fn network_policy_deny_and_allow() {
    let public = public_target();
    let ip = public.split(':').next().unwrap();
    let policy = NetworkPolicy {
        allow: vec![],
        deny: vec![ip.parse().unwrap()],
    };
    let out = run(
        &sandbox(policy),
        Network::Filtered,
        &probe(std::slice::from_ref(&public)),
    )
    .await;
    assert!(out.contains(&format!("{public} closed")), "{out}");

    let Some(lan) = host_lan_addr() else { return };
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let target = format!("{lan}:{}", listener.local_addr().unwrap().port());
    let policy = NetworkPolicy {
        allow: vec![sandbox::Ipv4Cidr::host(lan)],
        deny: vec![],
    };
    let out = run(
        &sandbox(policy),
        Network::Filtered,
        &probe(std::slice::from_ref(&target)),
    )
    .await;
    assert!(out.contains(&format!("{target} open")), "{out}");
}

#[tokio::test]
#[ignore = "needs bwrap and user namespaces"]
async fn no_network_has_no_egress() {
    let out = run(
        &sandbox(Default::default()),
        Network::None,
        &probe(&[public_target()]),
    )
    .await;
    assert!(out.contains(" closed") && !out.contains(" open"), "{out}");
}

const SPIN: &str = "exec -a ekaci-tree-marker bash -c 'while :; do :; done'";

#[tokio::test]
#[ignore = "needs bwrap, pasta and user namespaces"]
async fn filtered_timeout_kills_whole_tree() {
    let sb = sandbox(Default::default());
    let bash = sandbox::resolve_program("bash").unwrap();
    let spec = SandboxSpec::new(bash)
        .args(["-c", &format!("({SPIN}) & {SPIN}")])
        .network(Network::Filtered)
        .timeout(Duration::from_millis(1500));
    let mut child = sb.spawn(&spec).await.unwrap();
    let exit = child.wait().await.unwrap();
    assert_eq!(exit, SandboxExit::TimedOut(Duration::from_millis(1500)));
    std::thread::sleep(Duration::from_millis(500));
    let ps = std::process::Command::new("pgrep")
        .args(["-f", "ekaci-tree-marker"])
        .output()
        .unwrap();
    assert!(
        ps.stdout.is_empty(),
        "leftover: {}",
        String::from_utf8_lossy(&ps.stdout)
    );
}
