//! Destination vetting and redirect rules over a controlled resolver. No
//! test here opens a connection; the HTTP-level tests are in
//! `tests/remote_egress.rs`.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use super::*;

/// A resolver answering from a fixed table, counting lookups.
#[derive(Default)]
struct Table {
    answers: HashMap<String, Vec<IpAddr>>,
    lookups: Mutex<usize>,
}

impl Table {
    fn with(entries: &[(&str, IpAddr)]) -> Self {
        let mut answers: HashMap<String, Vec<IpAddr>> = HashMap::new();
        for (host, ip) in entries {
            answers.entry((*host).to_owned()).or_default().push(*ip);
        }
        Self {
            answers,
            lookups: Mutex::new(0),
        }
    }
}

#[async_trait]
impl Resolver for Table {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        *self.lookups.lock().unwrap() += 1;
        self.answers
            .get(host)
            .map(|ips| ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
            .ok_or_else(|| std::io::Error::other("no such host"))
    }
}

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

async fn denial(rules: &EgressRules, target: &str, resolver: &Table) -> Option<EgressDenial> {
    vet(rules, &url(target), resolver).await.err()
}

#[test]
fn networks_parse_and_contain() {
    let loopback: Network = "127.0.0.1/32".parse().unwrap();
    assert!(loopback.contains(v4(127, 0, 0, 1)));
    assert!(!loopback.contains(v4(127, 0, 0, 2)));
    let lan: Network = format!("{}/16", v4(10, 20, 0, 0)).parse().unwrap();
    assert!(lan.contains(v4(10, 20, 255, 1)));
    assert!(!lan.contains(v4(10, 21, 0, 1)));
    let v6: Network = "::1".parse().unwrap();
    assert!(v6.contains(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    assert!(!v6.contains(v4(127, 0, 0, 1)), "families never mix");
    for bad in ["", "host", "10.0.0.1/33", "::1/129", "10.0.0.1/x"] {
        assert!(bad.parse::<Network>().is_err(), "{bad}");
    }
}

#[tokio::test]
async fn every_non_public_and_management_destination_is_refused() {
    let rules = EgressRules::default();
    let none = Table::default();
    let metadata = v4(169, 254, 169, 254);
    let wireserver = v4(168, 63, 129, 16);
    let cases = [
        // Loopback, private, link-local, unspecified, multicast, CGNAT.
        (
            format!("http://{}/", v4(127, 0, 0, 1)),
            EgressDenial::NonPublicAddress,
        ),
        (
            format!("http://{}/", v4(10, 0, 0, 1)),
            EgressDenial::NonPublicAddress,
        ),
        (
            format!("http://{}/", v4(192, 168, 1, 1)),
            EgressDenial::NonPublicAddress,
        ),
        (
            format!("http://{}/", v4(169, 254, 1, 1)),
            EgressDenial::NonPublicAddress,
        ),
        (
            format!("http://{}/", v4(0, 0, 0, 0)),
            EgressDenial::NonPublicAddress,
        ),
        (
            format!("http://{}/", v4(224, 0, 0, 1)),
            EgressDenial::NonPublicAddress,
        ),
        ("http://[::1]/".to_owned(), EgressDenial::NonPublicAddress),
        (
            "http://[fe80::1]/".to_owned(),
            EgressDenial::NonPublicAddress,
        ),
        (
            "http://[fd00::1]/".to_owned(),
            EgressDenial::NonPublicAddress,
        ),
        (
            "http://[ff02::1]/".to_owned(),
            EgressDenial::NonPublicAddress,
        ),
        // IPv4-mapped IPv6 of loopback and of metadata.
        (
            "http://[::ffff:7f00:1]/".to_owned(),
            EgressDenial::NonPublicAddress,
        ),
        (
            "http://[::ffff:a9fe:a9fe]/".to_owned(),
            EgressDenial::ManagementEndpoint,
        ),
        // Alternate IPv4 notations of loopback, normalized by the URL parser.
        (
            "http://2130706433/".to_owned(),
            EgressDenial::NonPublicAddress,
        ),
        ("http://0x7f.1/".to_owned(), EgressDenial::NonPublicAddress),
        ("http://127.1/".to_owned(), EgressDenial::NonPublicAddress),
        (
            "http://0177.0.0.1/".to_owned(),
            EgressDenial::NonPublicAddress,
        ),
        // Cloud metadata and the Azure host agent (a public-range address).
        (
            format!("http://{metadata}/"),
            EgressDenial::ManagementEndpoint,
        ),
        (
            format!("http://{wireserver}/"),
            EgressDenial::ManagementEndpoint,
        ),
        (
            format!(
                "http://[{}]/",
                Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)
            ),
            EgressDenial::ManagementEndpoint,
        ),
    ];
    for (target, expected) in cases {
        assert_eq!(
            denial(&rules, &target, &none).await,
            Some(expected),
            "{target}"
        );
    }
    assert_eq!(*none.lookups.lock().unwrap(), 0, "literals need no DNS");
}

#[tokio::test]
async fn names_are_judged_on_every_resolved_address() {
    let rules = EgressRules {
        allow_domains: vec!["internal.test".to_owned(), "mixed.test".to_owned()],
        ..EgressRules::default()
    };
    let public = v4(8, 8, 8, 8);
    let table = Table::with(&[
        ("wiki.internal.test", v4(10, 0, 0, 1)),
        ("mixed.test", public),
        ("mixed.test", v4(127, 0, 0, 1)),
        ("meta.internal.test", v4(169, 254, 169, 254)),
        ("mapped.internal.test", "::ffff:127.0.0.1".parse().unwrap()),
    ]);
    // An allow-listed name resolving privately: the bypass this change closes.
    for (host, expected) in [
        ("wiki.internal.test", EgressDenial::NonPublicAddress),
        ("mixed.test", EgressDenial::NonPublicAddress),
        ("meta.internal.test", EgressDenial::ManagementEndpoint),
        ("mapped.internal.test", EgressDenial::NonPublicAddress),
    ] {
        assert_eq!(
            denial(&rules, &format!("https://{host}/"), &table).await,
            Some(expected),
            "{host}"
        );
    }
    assert_eq!(
        denial(&rules, "https://unknown.internal.test/", &table).await,
        Some(EgressDenial::ResolutionFailed)
    );
}

#[tokio::test]
async fn trusted_networks_are_an_operator_grant_only() {
    let wiki = v4(10, 0, 0, 1);
    let table = Table::with(&[
        ("wiki.corp.test", wiki),
        ("meta.test", v4(169, 254, 169, 254)),
    ]);
    let operator = EgressRules {
        trusted_networks: vec![format!("{wiki}/32").parse().unwrap()],
        ..EgressRules::default()
    };
    let vetted = vet(&operator, &url("http://wiki.corp.test/"), &table)
        .await
        .unwrap();
    assert_eq!(vetted.addrs, vec![SocketAddr::new(wiki, 80)]);
    assert!(vetted.named, "a name is pinned to its vetted address");
    // A broad trusted range never covers metadata; an exact entry does.
    let broad = EgressRules {
        trusted_networks: vec![format!("{}/16", v4(169, 254, 0, 0)).parse().unwrap()],
        ..EgressRules::default()
    };
    assert_eq!(
        denial(&broad, "http://meta.test/", &table).await,
        Some(EgressDenial::ManagementEndpoint)
    );
    let exact = EgressRules {
        trusted_networks: vec![format!("{}/32", v4(169, 254, 169, 254)).parse().unwrap()],
        ..EgressRules::default()
    };
    assert!(vet(&exact, &url("http://meta.test/"), &table).await.is_ok());
    // Trusting one network does not trust metadata.
    assert_eq!(
        denial(&operator, "http://meta.test/", &table).await,
        Some(EgressDenial::ManagementEndpoint)
    );
    // The same trusted network never applies to an untrusted run.
    let untrusted = EgressRules {
        untrusted_destinations: Some(vec!["wiki.corp.test".to_owned()]),
        ..operator.clone()
    };
    assert_eq!(
        denial(&untrusted, "http://wiki.corp.test/", &table).await,
        Some(EgressDenial::NonPublicAddress)
    );
}

#[tokio::test]
async fn the_legacy_switch_is_operator_only_and_never_reaches_management() {
    let open = EgressRules {
        block_private_addresses: false,
        ..EgressRules::default()
    };
    let none = Table::default();
    assert!(
        vet(&open, &url(&format!("http://{}/", v4(10, 0, 0, 1))), &none)
            .await
            .is_ok()
    );
    assert_eq!(
        denial(&open, &format!("http://{}/", v4(169, 254, 169, 254)), &none).await,
        Some(EgressDenial::ManagementEndpoint)
    );
    let untrusted = EgressRules {
        untrusted_destinations: Some(vec![v4(10, 0, 0, 1).to_string()]),
        ..open
    };
    assert_eq!(
        denial(&untrusted, &format!("http://{}/", v4(10, 0, 0, 1)), &none).await,
        Some(EgressDenial::NonPublicAddress)
    );
}

#[tokio::test]
async fn untrusted_runs_reach_only_their_profiles_destinations() {
    let public = v4(8, 8, 8, 8);
    let table = Table::with(&[
        ("docs.example.com", public),
        ("attacker.example.net", public),
    ]);
    let scoped = EgressRules {
        untrusted_destinations: Some(vec!["example.com".to_owned()]),
        ..EgressRules::default()
    };
    assert!(
        vet(&scoped, &url("https://docs.example.com/a"), &table)
            .await
            .is_ok()
    );
    assert_eq!(
        denial(&scoped, "https://attacker.example.net/?leak=1", &table).await,
        Some(EgressDenial::NotAuthorizedForProfile)
    );
    // An empty grant reaches nothing, however public.
    let none = EgressRules {
        untrusted_destinations: Some(Vec::new()),
        ..EgressRules::default()
    };
    assert_eq!(
        denial(&none, "https://docs.example.com/", &table).await,
        Some(EgressDenial::NotAuthorizedForProfile)
    );
    // The operator's global allow-list is not proof for an untrusted run.
    let global_only = EgressRules {
        allow_domains: vec!["attacker.example.net".to_owned()],
        untrusted_destinations: Some(Vec::new()),
        ..EgressRules::default()
    };
    assert_eq!(
        denial(&global_only, "https://attacker.example.net/", &table).await,
        Some(EgressDenial::NotAuthorizedForProfile)
    );
}

#[tokio::test]
async fn schemes_and_userinfo_are_refused() {
    let rules = EgressRules::default();
    let none = Table::default();
    for (target, expected) in [
        ("ftp://example.com/", EgressDenial::UnsupportedScheme),
        ("file:///etc/passwd", EgressDenial::UnsupportedScheme),
        ("https://user:secret@example.com/", EgressDenial::Userinfo),
        ("https://token@example.com/", EgressDenial::Userinfo),
    ] {
        assert_eq!(
            denial(&rules, target, &none).await,
            Some(expected),
            "{target}"
        );
    }
    // Denials carry no URL, path, query or secret.
    let message = EgressDenial::Userinfo.to_string();
    assert!(!message.contains("secret") && !message.contains("example"));
}

#[test]
fn redirects_are_validated_before_they_are_followed() {
    let start = url("https://api.example.com/a");
    // Same origin is followed.
    let mut anonymous = Redirects::new(&start, 5, false);
    let same = anonymous.next(&start, "/b").unwrap();
    assert_eq!(same.as_str(), "https://api.example.com/b");
    // A downgrade is refused, credentialed or not.
    assert_eq!(
        anonymous.next(&same, "http://api.example.com/c"),
        Err(EgressDenial::InsecureDowngrade)
    );
    // Cross-origin is allowed only for an uncredentialed request.
    assert!(anonymous.next(&same, "https://cdn.example.org/d").is_ok());
    let mut credentialed = Redirects::new(&start, 5, true);
    for target in [
        "https://evil.example.org/",
        "https://api.example.com:8443/",
        "https://other.example.com/",
    ] {
        assert_eq!(
            credentialed.next(&start, target),
            Err(EgressDenial::CrossOriginRedirect),
            "{target}"
        );
    }
    assert!(credentialed.next(&start, "/v2/a").is_ok());
    // Loops, schemes, userinfo, and the hop limit.
    let mut looping = Redirects::new(&start, 10, false);
    let b = looping.next(&start, "/b").unwrap();
    assert_eq!(looping.next(&b, "/a"), Err(EgressDenial::RedirectLoop));
    let mut odd = Redirects::new(&start, 10, false);
    assert_eq!(
        odd.next(&start, "file:///etc/passwd"),
        Err(EgressDenial::UnsupportedScheme)
    );
    assert_eq!(
        odd.next(&start, "https://user:pw@api.example.com/"),
        Err(EgressDenial::Userinfo)
    );
    let mut bounded = Redirects::new(&start, 2, false);
    let one = bounded.next(&start, "/1").unwrap();
    let two = bounded.next(&one, "/2").unwrap();
    assert_eq!(
        bounded.next(&two, "/3"),
        Err(EgressDenial::TooManyRedirects)
    );
}
