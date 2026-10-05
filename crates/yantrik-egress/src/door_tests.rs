//! Both doors, end to end: a real proxy on loopback, asked by this test's own account, with a
//! `CONNECT` and an absolute-form `GET` on each. Nothing here is reached: an allowed request ends
//! in 502 (the address did not answer, or the name did not resolve) or a tunnel, a refused one in
//! 403 before any connection is made — which is the difference under test.
//!
//! The proxy sees this machine's real network plus a home on a global IPv6 prefix and a public
//! IPv4 subnet ([`home`]), and answers a few names itself ([`names`]); every other name goes to the
//! system's resolver.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::door::Door;
use crate::ledger::Seen;
use crate::policy::{Mode, Policy, Rule};
use crate::local::Net;
use crate::proxy::{self, Proxy, World};
use crate::state::State;

struct Doors {
    endpoint: SocketAddr,
    public: SocketAddr,
    state: Arc<Mutex<State>>,
}

fn lan(host: &str, ports: &[u16]) -> Rule {
    Rule { host: host.into(), ports: ports.to_vec(), http: true, lan: true, why: "one of the Mind's endpoints".into(), seeded: true }
}

/// This machine's network, and on it: the ISP's global prefix 2a02:8070:abcd:1::/64, a public IPv4
/// subnet 81.2.69.160/28, and two routers outside both, 81.2.69.1 and 2a02:8070:abcd::1.
fn home() -> Option<Net> {
    let mut net = crate::local::addresses()
        .unwrap_or_else(|| Net { own: vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()], ..Net::default() });
    net.links.push(("2a02:8070:abcd:1::".parse().unwrap(), 64));
    net.links.push(("81.2.69.160".parse().unwrap(), 28));
    net.gateways.extend(["81.2.69.1".parse::<IpAddr>().unwrap(), "2a02:8070:abcd::1".parse().unwrap()]);
    Some(net)
}

fn names(host: &str) -> Option<Vec<IpAddr>> {
    let ips: &[&str] = match host {
        "nas.home.example" => &["2a02:8070:abcd:1::20"],
        "printer.home.example" => &["81.2.69.170"],
        "router.home.example" => &["81.2.69.1"],
        // A Pi-hole's answer for a blocked name.
        "sink.example.com" => &["0.0.0.0"],
        "nonexistent.invalid" => &[],
        _ => return None,
    };
    Some(ips.iter().map(|i| i.parse().unwrap()).collect())
}

async fn start(name: &str, policy: Policy) -> Doors {
    let dir = std::env::temp_dir().join(format!("yantrik-egress-doors-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(Mutex::new(State::load(&dir)));
    state.lock().unwrap().policy = policy;
    // SAFETY: getuid cannot fail.
    let serve_uid = unsafe { libc::getuid() };
    let open = Arc::new(tokio::sync::Semaphore::new(proxy::MOST_OPEN));
    let mut at = Vec::new();
    for door in [Door::Endpoint, Door::Public] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        at.push(local);
        tokio::spawn(proxy::serve(listener, Arc::new(Proxy { state: state.clone(), serve_uid, local, door, world: World { net: home, names } }), open.clone()));
    }
    Doors { endpoint: at[0], public: at[1], state }
}

/// The status code the proxy answers `request` with.
async fn status(door: SocketAddr, request: &str) -> u16 {
    let mut s = TcpStream::connect(door).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut got = Vec::new();
    let mut chunk = [0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut chunk).await {
                Ok(n) if n > 0 => got.extend_from_slice(&chunk[..n]),
                _ => break,
            }
        }
    })
    .await;
    let text = String::from_utf8_lossy(&got);
    text.strip_prefix("HTTP/1.1 ").and_then(|t| t.get(..3)).and_then(|c| c.parse().ok()).unwrap_or_else(|| panic!("no answer: {text:?}"))
}

fn connect(authority: &str) -> String {
    format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n")
}

fn get(authority: &str) -> String {
    format!("GET http://{authority}/x HTTP/1.1\r\nHost: {authority}\r\n\r\n")
}

/// Both kinds of request, at once.
async fn both(door: SocketAddr, authority: &str) -> [u16; 2] {
    let (c, g) = (connect(authority), get(authority));
    let (c, g) = tokio::join!(status(door, &c), status(door, &g));
    [c, g]
}

fn seen(d: &Doors, host: &str, port: u16) -> Seen {
    d.state.lock().unwrap().ledger.list().into_iter().find(|s| s.host == host && s.port == port).unwrap_or_else(|| panic!("{host}:{port} not counted"))
}

#[tokio::test]
async fn the_public_door_refuses_every_host_a_lan_rule_names() {
    for mode in Mode::ALL {
        let mut p = Policy { mode, rules: vec![] };
        p.seed(vec![lan("192.0.2.7", &[8888]), lan("gpu.example.invalid", &[11434]), lan("*.home.invalid", &[8123])]).unwrap();
        let d = start(&format!("lan-{mode:?}"), p).await;
        for authority in [
            "192.0.2.7:8888", "192.0.2.7:443", "gpu.example.invalid:11434", "gpu.example.invalid:443", "gpu.example.invalid:80",
            "ha.home.invalid:8123", "a.b.home.invalid:22",
        ] {
            assert_eq!(both(d.public, authority).await, [403, 403], "{mode:?} public: {authority}");
        }
        // The endpoint door is the proxy as it was: the lan rule lets the request past the policy
        // (nothing answers there, or the name does not resolve: 502), only on its ports.
        assert_eq!(both(d.endpoint, "192.0.2.7:8888").await, [502, 502], "{mode:?} endpoint: the lan rule");
        assert_eq!(both(d.endpoint, "gpu.example.invalid:11434").await, [502, 502], "{mode:?} endpoint: the lan name");
        assert_eq!(both(d.endpoint, "ha.home.invalid:8123").await, [502, 502], "{mode:?} endpoint: the lan wildcard");
        assert_eq!(both(d.endpoint, "192.0.2.7:22").await, [403, 403], "{mode:?} endpoint: not its port");
        let s = seen(&d, "192.0.2.7", 8888);
        assert_eq!(s.refused, 2, "{mode:?}: the public door's refusals are counted");
        assert_eq!(s.allowed + s.audited, 2, "{mode:?}: and the endpoint door's allowances");
        assert!(s.why.contains("public door"), "{:?}", s.why);
    }
}

#[tokio::test]
async fn the_public_door_refuses_what_is_not_the_internet() {
    let mut p = Policy::default();
    let literals = ["192.0.2.9", "198.51.100.9", "2001:db8::9"];
    p.seed(literals.iter().map(|h| lan(h, &[8080])).collect()).unwrap();
    let d = start("private", p).await;
    for authority in [
        "localhost:8080", "127.0.0.1:8080", "[::1]:8080", "10.0.0.7:8080", "172.16.0.7:8080", "192.168.4.42:8080",
        "169.254.169.254:80", "100.64.0.7:8080", "100.100.100.100:8080", "[fd00::5]:8080", "[fe80::1]:8080",
        "[::ffff:192.168.4.42]:8080", "[::ffff:127.0.0.1]:8080", "[::ffff:100.64.0.7]:8080", "[64:ff9b::c0a8:42a]:8080",
    ] {
        assert_eq!(both(d.public, authority).await, [403, 403], "public: {authority}");
    }
    // Literal addresses a lan rule names: past the policy on the endpoint door, never the public.
    for h in literals {
        let authority = if h.contains(':') { format!("[{h}]:8080") } else { format!("{h}:8080") };
        assert_eq!(both(d.public, &authority).await, [403, 403], "public: {authority}");
        assert_eq!(both(d.endpoint, &authority).await, [502, 502], "endpoint: {authority}");
    }
}

#[tokio::test]
async fn the_public_door_lets_the_internet_through() {
    // In audit with no rules: let through and counted on either door. Nothing need answer at
    // 1.1.1.1:9 (a 502), but if it does, the request went through (a tunnel, an answer).
    let d = start("internet", Policy::default()).await;
    for door in [d.public, d.endpoint] {
        for code in both(door, "1.1.1.1:9").await {
            assert_ne!(code, 403, "the internet is not refused");
        }
    }
    let s = seen(&d, "1.1.1.1", 9);
    assert_eq!((s.audited, s.refused), (4, 0));
    // In enforce, by a rule like any other.
    let mut p = Policy { mode: Mode::Enforce, rules: vec![] };
    p.allow(Rule { host: "1.1.1.1".into(), ports: vec![9], http: true, lan: false, why: "a public site".into(), seeded: false }).unwrap();
    let d = start("internet-enforce", p).await;
    for code in both(d.public, "1.1.1.1:9").await {
        assert_ne!(code, 403);
    }
    assert_eq!(both(d.public, "1.1.1.1:10").await, [403, 403], "enforce still asks for a rule");
    assert_eq!(seen(&d, "1.1.1.1", 9).allowed, 2);
}

#[tokio::test]
async fn guarded_lets_the_internet_through_both_doors_and_never_the_home_network() {
    let mut p = Policy { mode: Mode::Guarded, rules: vec![] };
    p.seed(vec![lan("192.0.2.7", &[8888])]).unwrap();
    let d = start("guarded", p).await;
    for door in [d.public, d.endpoint] {
        for code in both(door, "1.1.1.1:9").await {
            assert_ne!(code, 403, "the internet needs no rule in guarded");
        }
        for authority in ["127.0.0.1:8080", "10.0.0.7:8080", "192.168.4.42:8080", "[fd00::5]:8080", "169.254.169.254:80"] {
            assert_eq!(both(door, authority).await, [403, 403], "no rule: {authority}");
        }
    }
    let s = seen(&d, "1.1.1.1", 9);
    assert_eq!((s.allowed, s.audited, s.refused), (4, 0, 0), "counted as allowed, not watched");
    // The lan rule opens its host on the endpoint door only.
    assert_eq!(both(d.endpoint, "192.0.2.7:8888").await, [502, 502], "endpoint: the lan rule");
    assert_eq!(both(d.public, "192.0.2.7:8888").await, [403, 403], "public: never");
    assert_eq!(both(d.endpoint, "192.0.2.7:22").await, [403, 403], "endpoint: not its port");
    let proposals: Vec<String> = { let s = d.state.lock().unwrap(); s.ledger.proposals(&s.policy) }.into_iter().map(|s| s.host).collect();
    assert!(!proposals.iter().any(|h| h == "1.1.1.1"), "nothing public is a proposal: {proposals:?}");
}

fn proposals(d: &Doors) -> Vec<String> {
    let s = d.state.lock().unwrap();
    s.ledger.proposals(&s.policy).into_iter().map(|s| s.host).collect()
}

/// A home device on the ISP's global IPv6 prefix, on a public IPv4 subnet this machine is on, or
/// the router itself, is the home network: refused without a lan rule in every mode, and on the
/// public door always.
#[tokio::test]
async fn a_home_device_on_a_global_prefix_or_the_router_is_the_home_network() {
    let home_devices = [
        "nas.home.example:445", "[2a02:8070:abcd:1::20]:445", "[2a02:8070:abcd:1::99]:5000", "printer.home.example:631",
        "81.2.69.170:631", "[::ffff:81.2.69.170]:631", "[64:ff9b::5102:45aa]:631", "router.home.example:80", "81.2.69.1:80",
        "[2a02:8070:abcd::1]:443",
    ];
    for mode in Mode::ALL {
        let d = start(&format!("home-{mode:?}"), Policy { mode, rules: vec![] }).await;
        for door in [d.endpoint, d.public] {
            for authority in home_devices {
                assert_eq!(both(door, authority).await, [403, 403], "{mode:?}: {authority}");
            }
        }
        if mode != Mode::Enforce {
            let s = seen(&d, "nas.home.example", 445);
            assert!(s.lan && s.why.contains("local network"), "{mode:?}: counted as the home network: {s:?}");
            assert!(proposals(&d).iter().any(|h| h == "nas.home.example"), "{mode:?}: and asked about");
        }
    }
    // In guarded, a rule without lan does not open it; one with lan does, on the endpoint door only.
    let mut p = Policy { mode: Mode::Guarded, rules: vec![] };
    p.allow(Rule { host: "nas.home.example".into(), ports: vec![445], http: true, lan: false, why: "files".into(), seeded: false }).unwrap();
    let d = start("home-guarded-rule", p).await;
    for door in [d.endpoint, d.public] {
        assert_eq!(both(door, "nas.home.example:445").await, [403, 403], "a rule without lan");
    }
    let mut p = Policy { mode: Mode::Guarded, rules: vec![] };
    p.seed(vec![lan("nas.home.example", &[445])]).unwrap();
    let d = start("home-guarded-lan", p).await;
    assert_eq!(both(d.endpoint, "nas.home.example:445").await, [502, 502], "endpoint: the lan rule");
    assert_eq!(both(d.public, "nas.home.example:445").await, [403, 403], "public: never");
    // The internet is still the internet.
    for code in both(d.public, "1.1.1.1:9").await {
        assert_ne!(code, 403);
    }
}

/// What no rule could reach is counted but never asked about: a name that did not resolve, one
/// that resolves to a sinkhole's 0.0.0.0, and this machine's own ports.
#[tokio::test]
async fn what_no_rule_could_reach_is_never_a_proposal() {
    for mode in Mode::ALL {
        let d = start(&format!("barred-{mode:?}"), Policy { mode, rules: vec![] }).await;
        let want = if mode == Mode::Enforce { 403 } else { 502 };
        assert_eq!(both(d.endpoint, "nonexistent.invalid:443").await, [want, want], "{mode:?}");
        for authority in ["sink.example.com:443", "127.0.0.1:7450", "[::1]:7450", "0.0.0.0:443"] {
            assert_eq!(both(d.endpoint, authority).await, [403, 403], "{mode:?}: {authority}");
        }
        assert_eq!(both(d.endpoint, "192.168.4.42:8080").await, [403, 403], "{mode:?}");
        let mut want = vec!["192.168.4.42"];
        if mode == Mode::Enforce {
            // Refused unresolved, for want of a rule: a rule could answer these.
            want.extend(["nonexistent.invalid", "sink.example.com"]);
        }
        let mut got = proposals(&d);
        got.sort();
        want.sort();
        assert_eq!(got, want, "{mode:?}");
        assert_eq!(seen(&d, "127.0.0.1", 7450).refused, 2, "{mode:?}: still counted");
    }
}

#[tokio::test]
async fn private_mode_closes_both_doors() {
    for mode in Mode::ALL {
        let d = start(&format!("private-mode-{mode:?}"), Policy { mode, rules: vec![] }).await;
        d.state.lock().unwrap().private = true;
        for door in [d.endpoint, d.public] {
            assert_eq!(both(door, "1.1.1.1:9").await, [403, 403], "{mode:?}");
        }
    }
}
