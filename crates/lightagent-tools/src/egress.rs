//! Outbound destination policy for the remote tools.
//!
//! Every hop of a remote request is checked here, independently of whether
//! the tool itself was permitted:
//!
//! ```text
//! tool permitted (policy.rs) → credential granted (caller) → destination
//! vetted here → connection pinned to the vetted addresses, no proxy, no
//! automatic redirect → each redirect re-checked here before it is followed
//! ```
//!
//! [`vet`] checks the scheme, refuses userinfo, applies the run's destination
//! scope (a profile's authorized hosts for an untrusted run, the operator's
//! `allow_domains` otherwise), resolves the host through an injectable
//! [`Resolver`], and judges **every resolved address**: an allow-listed name
//! must still resolve to a permitted address. A non-public address is
//! permitted only inside an operator-approved trusted network, and never
//! for an untrusted run. Management endpoints (cloud metadata, the Azure
//! host agent) are refused unless an operator's trusted-network entry names
//! that exact address; a broad trusted range never covers them.
//!
//! [`pinned_client`] connects only to the vetted addresses (the TLS name and
//! SNI stay the URL's host), with automatic redirects off and every proxy
//! disabled, so neither a second DNS answer nor an environment proxy can
//! move the connection. [`Redirects`] validates each redirect: a hop count,
//! loop detection, http(s) only, no userinfo, no HTTPS-to-HTTP downgrade,
//! and no cross-origin hop at all for a request that carries a credential.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use serde::Serialize;

use crate::builtins::web::is_public_ip;

/// Why a destination was refused. Codes only: no URL, path, query or
/// credential is ever carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum EgressDenial {
    #[error("only http and https destinations are allowed")]
    UnsupportedScheme,
    #[error("destinations must not carry credentials in the URL")]
    Userinfo,
    #[error("the destination has no host")]
    NoHost,
    #[error("the destination is not in web.allow_domains")]
    NotInAllowList,
    #[error("the destination is not authorized for this profile")]
    NotAuthorizedForProfile,
    #[error("the destination resolves to a non-public address")]
    NonPublicAddress,
    #[error("the destination is a cloud metadata or host management endpoint")]
    ManagementEndpoint,
    #[error("the destination could not be resolved")]
    ResolutionFailed,
    #[error("the destination resolved to no address")]
    NoAddress,
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("the redirects form a loop")]
    RedirectLoop,
    #[error("a credentialed request may not be redirected to another origin")]
    CrossOriginRedirect,
    #[error("a redirect from https to http is not followed")]
    InsecureDowngrade,
    #[error("the redirect target is invalid")]
    InvalidRedirect,
}

/// An IP network in CIDR form, for operator-approved trusted networks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Network {
    base: IpAddr,
    prefix: u8,
}

impl std::str::FromStr for Network {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let base: IpAddr = address
            .parse()
            .map_err(|_| format!("{text:?} is not an IP network"))?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix
                .parse::<u8>()
                .ok()
                .filter(|prefix| *prefix <= max)
                .ok_or_else(|| format!("{text:?} has an invalid prefix length"))?,
            None => max,
        };
        Ok(Self { base, prefix })
    }
}

impl Network {
    /// Whether this network is exactly the one address `ip`.
    pub fn is_single(&self, ip: IpAddr) -> bool {
        self.base == ip && self.prefix == if ip.is_ipv4() { 32 } else { 128 }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        fn bits(ip: IpAddr) -> (u128, u8) {
            match ip {
                IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
                IpAddr::V6(v6) => (u128::from(v6), 128),
            }
        }
        let (base, width) = bits(self.base);
        let (candidate, candidate_width) = bits(ip);
        if width != candidate_width {
            return false;
        }
        if self.prefix == 0 {
            return true;
        }
        let shift = u32::from(width - self.prefix);
        (base >> shift) == (candidate >> shift)
    }
}

/// Cloud metadata and host management endpoints. Refused even when private
/// addresses are otherwise allowed, unless a trusted-network entry names the
/// exact address.
pub fn is_management_endpoint(ip: IpAddr) -> bool {
    let v4 = match ip {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped(),
    };
    if let Some(v4) = v4 {
        // Link-local metadata (AWS, GCP, Azure IMDS, OpenStack), the Azure
        // WireServer host agent (a public-range address, so the public check
        // alone would allow it), Alibaba's metadata service, and Oracle's.
        let [a, b, c, d] = v4.octets();
        return (a, b, c, d) == (169, 254, 169, 254)
            || (a, b, c, d) == (169, 254, 170, 2)
            || (a, b, c, d) == (168, 63, 129, 16)
            || (a, b, c, d) == (100, 100, 100, 200)
            || (a, b, c, d) == (192, 0, 0, 192);
    }
    // AWS IMDS over IPv6 (segments below; unique-local, so also non-public).
    matches!(ip, IpAddr::V6(v6) if v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254))
}

/// Resolves host names for [`vet`]. Injectable so tests control DNS.
#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>>;
}

/// The operating system's resolver.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        Ok(tokio::net::lookup_host((host, port)).await?.collect())
    }
}

/// The destination scope of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressRules {
    /// The operator's host allow-list; empty allows any host (subject to the
    /// address checks). Never proof of authorization for an untrusted run.
    pub allow_domains: Vec<String>,
    /// Operator-approved networks a non-public address may be reached in.
    /// Never applied to an untrusted run.
    pub trusted_networks: Vec<Network>,
    /// The legacy operator switch. When false, an operator run may reach
    /// non-public addresses (never management endpoints). Ignored for an
    /// untrusted run.
    pub block_private_addresses: bool,
    /// `Some` for an untrusted run: the hosts its profile is authorized to
    /// reach. An empty list reaches nothing.
    pub untrusted_destinations: Option<Vec<String>>,
}

impl Default for EgressRules {
    fn default() -> Self {
        Self {
            allow_domains: Vec::new(),
            trusted_networks: Vec::new(),
            block_private_addresses: true,
            untrusted_destinations: None,
        }
    }
}

/// Whether `host` equals, or is a subdomain of, one of `domains`.
pub fn host_matches(host: &str, domains: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    domains.iter().any(|domain| {
        let domain = domain
            .trim()
            .trim_start_matches('.')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
    })
}

/// A destination that passed [`vet`]: connect only to `addrs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vetted {
    pub url: Url,
    /// The URL host as written for TLS and SNI (no brackets).
    pub host: String,
    pub addrs: Vec<SocketAddr>,
    /// Whether the host is a name (and so must be pinned) rather than an IP.
    pub named: bool,
}

impl EgressRules {
    fn address_permitted(&self, ip: IpAddr) -> Result<(), EgressDenial> {
        let operator = self.untrusted_destinations.is_none();
        if is_management_endpoint(ip) {
            // Only a trusted-network entry naming exactly this address
            // reaches a management endpoint; a broad range never does.
            let named = operator
                && self
                    .trusted_networks
                    .iter()
                    .any(|network| network.is_single(ip));
            return if named {
                Ok(())
            } else {
                Err(EgressDenial::ManagementEndpoint)
            };
        }
        if operator
            && self
                .trusted_networks
                .iter()
                .any(|network| network.contains(ip))
        {
            return Ok(());
        }
        if is_public_ip(ip) {
            return Ok(());
        }
        if self.untrusted_destinations.is_none() && !self.block_private_addresses {
            return Ok(());
        }
        Err(EgressDenial::NonPublicAddress)
    }
}

/// Check `url` against `rules`, resolving its host through `resolver`, and
/// return the addresses the connection must use.
pub async fn vet(
    rules: &EgressRules,
    url: &Url,
    resolver: &dyn Resolver,
) -> Result<Vetted, EgressDenial> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(EgressDenial::UnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EgressDenial::Userinfo);
    }
    // The URL parser has already normalized alternate IPv4 notations
    // (decimal, octal, hexadecimal, shortened) to dotted form.
    let Some(raw) = url.host_str().filter(|host| !host.is_empty()) else {
        return Err(EgressDenial::NoHost);
    };
    let unbracketed = raw
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(raw);
    let literal = unbracketed.parse::<IpAddr>().ok();
    let host = unbracketed.to_ascii_lowercase();
    if let Some(authorized) = &rules.untrusted_destinations
        && !host_matches(&host, authorized)
    {
        return Err(EgressDenial::NotAuthorizedForProfile);
    }
    if !rules.allow_domains.is_empty() && !host_matches(&host, &rules.allow_domains) {
        return Err(EgressDenial::NotInAllowList);
    }
    let port = url.port_or_known_default().unwrap_or(0);
    let addrs = match literal {
        Some(ip) => vec![SocketAddr::new(ip, port)],
        None => resolver
            .resolve(&host, port)
            .await
            .map_err(|_| EgressDenial::ResolutionFailed)?,
    };
    if addrs.is_empty() {
        return Err(EgressDenial::NoAddress);
    }
    for addr in &addrs {
        rules.address_permitted(addr.ip())?;
    }
    Ok(Vetted {
        url: url.clone(),
        host,
        addrs,
        named: literal.is_none(),
    })
}

/// A client that reaches only `vetted`'s addresses: the name is pinned (no
/// second resolution), automatic redirects are off, and no proxy, from the
/// environment or the system, is used. TLS still verifies the URL's host.
pub fn pinned_client(vetted: &Vetted, timeout: Duration) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .user_agent(concat!("lightagent/", env!("CARGO_PKG_VERSION")));
    if vetted.named {
        builder = builder.resolve_to_addrs(&vetted.host, &vetted.addrs);
    }
    builder
        .build()
        .map_err(|_| "could not build the outbound client".to_owned())
}

/// The scheme, host and port of a URL.
fn origin(url: &Url) -> (String, Option<String>, Option<u16>) {
    (
        url.scheme().to_owned(),
        url.host_str().map(str::to_ascii_lowercase),
        url.port_or_known_default(),
    )
}

/// Follows redirects under policy: each target is validated here before it
/// is vetted and fetched like the first.
#[derive(Debug)]
pub struct Redirects {
    start: (String, Option<String>, Option<u16>),
    visited: HashSet<String>,
    hops: u32,
    max: u32,
    credentialed: bool,
}

impl Redirects {
    /// `credentialed`: the request carries a credential, so it may never
    /// leave its first origin.
    pub fn new(start: &Url, max: u32, credentialed: bool) -> Self {
        let mut visited = HashSet::new();
        visited.insert(without_fragment(start));
        Self {
            start: origin(start),
            visited,
            hops: 0,
            max,
            credentialed,
        }
    }

    /// The next URL for a redirect from `current` to `location`, or why it is
    /// refused.
    pub fn next(&mut self, current: &Url, location: &str) -> Result<Url, EgressDenial> {
        self.hops += 1;
        if self.hops > self.max {
            return Err(EgressDenial::TooManyRedirects);
        }
        let next = current
            .join(location)
            .map_err(|_| EgressDenial::InvalidRedirect)?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err(EgressDenial::UnsupportedScheme);
        }
        if !next.username().is_empty() || next.password().is_some() {
            return Err(EgressDenial::Userinfo);
        }
        if current.scheme() == "https" && next.scheme() == "http" {
            return Err(EgressDenial::InsecureDowngrade);
        }
        if self.credentialed && origin(&next) != self.start {
            return Err(EgressDenial::CrossOriginRedirect);
        }
        if !self.visited.insert(without_fragment(&next)) {
            return Err(EgressDenial::RedirectLoop);
        }
        Ok(next)
    }
}

fn without_fragment(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

/// The host of `url` for a message: never its path, query or userinfo.
pub fn display_host(url: &Url) -> String {
    url.host_str().unwrap_or("?").to_owned()
}

#[cfg(test)]
mod tests;
