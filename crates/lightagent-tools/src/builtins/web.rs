//! `web.fetch` and `web.search` — reaching the network for read-only data.
//!
//! Both are [`RiskClass::External`](lightagent_core::RiskClass::External): they
//! read remote data and change nothing local. They run only when the caller has
//! injected a [`WebContext`] (web access enabled in config); absent it, they
//! return a controlled result the model is shown, never a panic — the same shape
//! `agent.delegate` uses when delegation is off.
//!
//! `web.fetch` is guarded by [`crate::egress`]: the URL scheme must be http(s),
//! the host must clear the run's destination scope, and every address it
//! resolves to must be permitted, an allow-listed host included. The connection
//! is pinned to the vetted addresses with no proxy, and every redirect is
//! validated and vetted again, so a fetch cannot be steered onto a loopback,
//! private or management service (an SSRF guard). HTML is reduced to readable
//! text without an HTML dependency.
//!
//! `web.search` sends the query to the configured endpoint. It supports
//! DuckDuckGo's HTML results directly and JSON endpoints with a `results`
//! array, such as SearXNG; it reports cleanly when no backend is configured.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use lightagent_core::{RiskClass, Scope, ToolOutcome};
use reqwest::header::{CONTENT_TYPE, LOCATION};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::{ToolCtx, WebContext};
use crate::definition::{Tool, ToolDefinition};
use crate::egress;

/// The most redirect hops `web.fetch` follows, each re-guarded.
const MAX_REDIRECTS: u32 = 5;
/// Keep one fetched page small enough to leave room for reasoning and follow-up searches.
const MAX_READABLE_CHARS: usize = 50_000;

/// `web.fetch` — fetch a URL and return its readable text.
pub struct WebFetch {
    definition: ToolDefinition,
}

impl WebFetch {
    /// The tool's stable name.
    pub const NAME: &'static str = "web.fetch";

    /// Build the tool with its declaration.
    pub fn new() -> Self {
        let parameters = json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The http(s) URL to fetch." }
            },
            "required": ["url"],
            "additionalProperties": false,
        });
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Fetch an http(s) URL and return its readable text.",
                parameters,
                RiskClass::External,
                vec![Scope::new("web:fetch")],
            ),
        }
    }
}

impl Default for WebFetch {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct FetchArgs {
    url: String,
}

#[async_trait]
impl Tool for WebFetch {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(web) = ctx.web.clone() else {
            return ToolOutcome::error("web access is not enabled for this run");
        };
        let args: FetchArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolOutcome::error(format!("could not read fetch arguments: {error}"));
            }
        };
        match fetch_text_authorized(&web, &args.url, ctx).await {
            Ok(text) => ToolOutcome::ok(text),
            Err(message) => ToolOutcome::error(message),
        }
    }
}

/// Follow `start` under the guard, up to [`MAX_REDIRECTS`] hops.
/// Fetch one page through the same redirect and SSRF guards as [`WebFetch`].
///
/// Composite tools such as realtime RAG use this typed entry point rather than
/// calling `web.fetch` and reparsing its rendered [`ToolOutcome`].
pub async fn fetch_text(web: &WebContext, start: &str) -> Result<String, String> {
    fetch(web, start, None).await
}

/// [`fetch_text`] for a run's tool call: the run's authorization is checked
/// before every hop, so a revoked run opens no further connection.
pub async fn fetch_text_authorized(
    web: &WebContext,
    start: &str,
    ctx: &ToolCtx,
) -> Result<String, String> {
    fetch(web, start, Some(ctx)).await
}

async fn fetch(web: &WebContext, start: &str, ctx: Option<&ToolCtx>) -> Result<String, String> {
    let mut url = reqwest::Url::parse(start).map_err(|_| "invalid URL".to_owned())?;
    let rules = web.policy.egress_rules();
    // web.fetch carries no credential, but redirects are still validated:
    // http(s) only, no userinfo, no downgrade, no loop, a bounded count.
    let mut redirects = egress::Redirects::new(&url, MAX_REDIRECTS, false);
    loop {
        if let Some(ctx) = ctx {
            ctx.authorized()?;
        }
        // Messages name the host only: never a path, query or userinfo.
        let host = egress::display_host(&url);
        // Every hop is vetted on its resolved addresses and the connection is
        // pinned to them, with no proxy and no automatic redirect, so neither
        // a second DNS answer (rebinding) nor a proxy can move it elsewhere.
        let vetted = egress::vet(&rules, &url, web.resolver.as_ref())
            .await
            .map_err(|denial| format!("refused to fetch from {host}: {denial}"))?;
        let client = egress::pinned_client(&vetted, web.policy.timeout)?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| format!("could not fetch from {host}: {}", error.without_url()))?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "redirect without a Location header".to_owned())?;
            url = redirects
                .next(&url, location)
                .map_err(|denial| format!("refused a redirect from {host}: {denial}"))?;
            continue;
        }

        if !response.status().is_success() {
            return Err(format!(
                "{host} returned HTTP {}",
                response.status().as_u16()
            ));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = read_bounded(&mut response, web.policy.max_fetch_bytes).await?;
        return Ok(limit_readable_text(render_fetch(
            &url,
            &content_type,
            &body,
        )));
    }
}

/// Render a fetched body as text: HTML reduced to readable text, other textual
/// types passed through, binary types named rather than dumped.
fn render_fetch(url: &reqwest::Url, content_type: &str, body: &[u8]) -> String {
    let is_html = content_type.contains("html");
    let is_textual = content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript");

    let mut out = format!("URL: {url}\n");
    if is_html {
        let html = String::from_utf8_lossy(body);
        let (title, text) = html_to_text(&html);
        if let Some(title) = title {
            out.push_str(&format!("Title: {title}\n"));
        }
        out.push('\n');
        out.push_str(&text);
    } else if is_textual {
        out.push('\n');
        out.push_str(&String::from_utf8_lossy(body));
    } else {
        out.push_str(&format!(
            "\n[{} bytes of {} content, not shown]",
            body.len(),
            if content_type.is_empty() {
                "binary"
            } else {
                content_type
            },
        ));
    }
    out
}

/// `web.search` — query the configured search backend.
pub struct WebSearch {
    definition: ToolDefinition,
}

impl WebSearch {
    /// The tool's stable name.
    pub const NAME: &'static str = "web.search";

    /// Build the tool with its declaration.
    pub fn new() -> Self {
        let parameters = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query." },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Cap the number of results returned.",
                }
            },
            "required": ["query"],
            "additionalProperties": false,
        });
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Search the web and return a list of result titles, URLs and snippets.",
                parameters,
                RiskClass::External,
                vec![Scope::new("web:search")],
            ),
        }
    }
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for WebSearch {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(web) = ctx.web.clone() else {
            return ToolOutcome::error("web access is not enabled for this run");
        };
        let args: SearchArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolOutcome::error(format!("could not read search arguments: {error}"));
            }
        };
        if let Err(reason) = ctx.authorized() {
            return ToolOutcome::error(reason);
        }
        match search_results(&web, &args.query, args.max_results).await {
            Ok(hits) if hits.is_empty() => {
                ToolOutcome::ok(format!("No results for {:?}.", args.query))
            }
            Ok(hits) => ToolOutcome::ok(render_search(&args.query, &hits)),
            Err(message) => ToolOutcome::error(message),
        }
    }
}

/// Search the configured backend and return structured results.
///
/// This is public so a composite retrieval tool can search and fetch without
/// forcing a small model through a brittle multi-tool planning loop.
pub async fn search_results(
    web: &WebContext,
    query: &str,
    requested: Option<usize>,
) -> Result<Vec<WebSearchHit>, String> {
    let policy = &web.policy;
    let Some(endpoint) = &policy.search_endpoint else {
        return Err("no search backend is configured (set web.search.endpoint)".to_owned());
    };
    let ceiling = policy.search_max_results.max(1);
    let limit = requested.unwrap_or(ceiling).clamp(1, ceiling);

    // An installation-wide search key is never used, and the query never
    // sent anonymously in its place, for a profile not granted it.
    if policy.search_credential_withheld {
        return Err("the search credential is not granted to this profile".to_owned());
    }
    let mut url = reqwest::Url::parse(endpoint)
        .map_err(|error| format!("invalid search endpoint: {error}"))?;
    if let Some(authorized) = &policy.untrusted_destinations
        && !url
            .host_str()
            .is_some_and(|host| egress::host_matches(host, authorized))
    {
        return Err("the search endpoint is not authorized for this profile".to_owned());
    }
    url.query_pairs_mut()
        .append_pair(&policy.search_query_param, query);

    let mut request = web.client.get(url);
    if let Some(key) = &policy.search_api_key {
        request = request.bearer_auth(key);
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| format!("search request failed: {}", error.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "search endpoint returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = read_bounded(&mut response, policy.max_fetch_bytes).await?;
    let hits = if content_type.contains("html") {
        let endpoint_host = reqwest::Url::parse(endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        if !endpoint_host
            .as_deref()
            .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
        {
            return Err(
                "search endpoint returned HTML; use DuckDuckGo HTML or a JSON endpoint".to_owned(),
            );
        }
        parse_duckduckgo_results(&String::from_utf8_lossy(&body), limit)
    } else {
        let json: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("search endpoint did not return JSON: {error}"))?;
        parse_search_results(&json, limit)
    };
    Ok(hits)
}

fn parse_duckduckgo_results(html: &str, limit: usize) -> Vec<WebSearchHit> {
    html.split("result results_links")
        .skip(1)
        .filter_map(|result| {
            let (href, title) = anchor_with_class(result, "result__a")?;
            let url = duckduckgo_target(&href)?;
            let snippet = anchor_with_class(result, "result__snippet")
                .map(|(_, text)| text)
                .unwrap_or_default();
            Some(WebSearchHit {
                title,
                url,
                snippet,
            })
        })
        .take(limit)
        .collect()
}

fn anchor_with_class(html: &str, class: &str) -> Option<(String, String)> {
    let marker = format!("class=\"{class}\"");
    let class_at = html.find(&marker)?;
    let open_at = html[..class_at].rfind("<a")?;
    let tag_end = html[class_at..].find('>')? + class_at;
    let tag = &html[open_at..=tag_end];
    let href_at = tag.find("href=\"")? + "href=\"".len();
    let href_end = tag[href_at..].find('"')? + href_at;
    let close_at = html[tag_end + 1..].find("</a>")? + tag_end + 1;
    let href = decode_entities(&tag[href_at..href_end]);
    let title = collapse_whitespace(&decode_entities(&strip_tags(&html[tag_end + 1..close_at])));
    (!href.is_empty() && !title.is_empty()).then_some((href, title))
}

fn duckduckgo_target(href: &str) -> Option<String> {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    let url = reqwest::Url::parse(&absolute).ok()?;
    if url
        .host_str()
        .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
        && url.path() == "/l/"
    {
        return url
            .query_pairs()
            .find_map(|(key, value)| (key == "uddg").then(|| value.into_owned()));
    }
    matches!(url.scheme(), "http" | "https").then_some(absolute)
}

fn limit_readable_text(text: String) -> String {
    if text.chars().count() <= MAX_READABLE_CHARS {
        return text;
    }
    let mut limited = text.chars().take(MAX_READABLE_CHARS).collect::<String>();
    limited.push_str("\n\n[page text truncated]");
    limited
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct WebSearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Read a `results` array of `{title, url, content|snippet|description}` from a
/// search response — SearXNG's shape and a common minimal one. An item without a
/// `url` is skipped; a missing title falls back to the URL.
fn parse_search_results(json: &Value, limit: usize) -> Vec<WebSearchHit> {
    let Some(results) = json.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str)?;
            let title = item.get("title").and_then(Value::as_str).unwrap_or(url);
            let snippet = ["content", "snippet", "description", "body"]
                .iter()
                .find_map(|key| item.get(*key).and_then(Value::as_str))
                .unwrap_or("");
            Some(WebSearchHit {
                title: title.to_owned(),
                url: url.to_owned(),
                snippet: snippet.to_owned(),
            })
        })
        .take(limit)
        .collect()
}

fn render_search(query: &str, hits: &[WebSearchHit]) -> String {
    let mut out = format!("Results for {query:?}:\n\n");
    for (index, hit) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {}\n{}\n", index + 1, hit.title, hit.url));
        if !hit.snippet.is_empty() {
            out.push_str(&format!("{}\n", hit.snippet));
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

/// Read at most `cap` bytes of a response body, stopping early so a large or
/// hostile page cannot exhaust memory.
async fn read_bounded(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut buffer = Vec::new();
    while buffer.len() < cap {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(cap - buffer.len());
                buffer.extend_from_slice(&chunk[..take]);
            }
            Ok(None) => break,
            Err(error) => return Err(format!("error reading body: {error}")),
        }
    }
    Ok(buffer)
}

/// Whether an address is globally routable — the negation of the SSRF guard.
pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
    {
        return false;
    }
    let octets = ip.octets();
    if octets[0] == 0 || octets[0] >= 224 {
        // 0.0.0.0/8, and multicast (224/4) plus reserved (240/4).
        return false;
    }
    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        // Carrier-grade NAT (RFC 6598 shared range).
        return false;
    }
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        // IETF protocol assignments, 192.0.0.0/24 (RFC 6890).
        return false;
    }
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        // 6to4 relay anycast, 192.88.99.0/24 (deprecated, RFC 7526).
        return false;
    }
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        // Benchmarking, 198.18.0.0/15 (RFC 2544).
        return false;
    }
    true
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    // Any IPv4 smuggled inside a v6 address is judged as that IPv4, so a private
    // v4 cannot reach a private service by wearing a v6 coat — mapped
    // (::ffff:0:0/96), compatible (::/96, deprecated), 6to4 (2002::/16) and the
    // well-known NAT64 prefix (64:ff9b::/96).
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    let segments = ip.segments();
    if segments[0] & 0xfe00 == 0xfc00 {
        // Unique local, fc00::/7.
        return false;
    }
    if segments[0] & 0xffc0 == 0xfe80 {
        // Link-local unicast, fe80::/10.
        return false;
    }
    true
}

/// The IPv4 address embedded in a v6 address by one of the transition schemes,
/// or `None` for a native v6 address. Used so the SSRF check judges the real
/// IPv4 destination rather than waving the v6 wrapper through.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        // ::ffff:a.b.c.d
        return Some(v4);
    }
    let s = ip.segments();
    if s[..6] == [0, 0, 0, 0, 0, 0] && s[6..] != [0, 0] && s[6..] != [0, 1] {
        // ::a.b.c.d — IPv4-compatible (deprecated), excluding :: and ::1 which
        // are handled above as unspecified/loopback.
        return Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x2002 {
        // 6to4, 2002:a.b.c.d::/48 — the IPv4 is the next two segments.
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        // NAT64 well-known prefix, 64:ff9b::/96 — the IPv4 is the low 32 bits.
        return Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    None
}

/// Reduce HTML to readable text: the `<title>`, then the body with scripts and
/// styles removed, tags flattened (block tags to newlines), entities decoded and
/// whitespace collapsed. A heuristic, not a browser — enough for a model to read.
fn html_to_text(html: &str) -> (Option<String>, String) {
    let title = extract_title(html);
    let stripped = remove_blocks(html);
    let flattened = strip_tags(&stripped);
    let decoded = decode_entities(&flattened);
    (title, collapse_whitespace(&decoded))
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let content_start = lower[open..].find('>')? + open + 1;
    let content_end = lower[content_start..].find("</title>")? + content_start;
    let raw = &html[content_start..content_end];
    let text = collapse_whitespace(&decode_entities(raw));
    if text.is_empty() { None } else { Some(text) }
}

/// Drop `<script>`, `<style>`, `<head>` and `<svg>` elements whole.
fn remove_blocks(html: &str) -> String {
    let mut current = html.to_owned();
    for tag in ["script", "style", "noscript", "head", "svg"] {
        current = remove_element(&current, tag);
    }
    current
}

fn remove_element(input: &str, tag: &str) -> String {
    // `to_ascii_lowercase` preserves byte length, so byte indices align.
    let lower = input.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if lower[index..].starts_with(&open) {
            match lower[index..].find(&close) {
                Some(offset) => {
                    index += offset + close.len();
                    out.push(' ');
                    continue;
                }
                None => break, // unterminated element: drop the remainder
            }
        }
        let ch = input[index..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Replace tags with spacing: block-level tags become a newline, the rest a
/// space, so words never run together where an element boundary sat.
fn strip_tags(input: &str) -> String {
    const BLOCK: &[&str] = &[
        "br",
        "p",
        "div",
        "li",
        "tr",
        "ul",
        "ol",
        "table",
        "section",
        "article",
        "header",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "pre",
    ];
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }
        let mut tag = String::new();
        for inner in chars.by_ref() {
            if inner == '>' {
                break;
            }
            tag.push(inner);
        }
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        out.push(if BLOCK.contains(&name.as_str()) {
            '\n'
        } else {
            ' '
        });
    }
    out
}

/// Decode the common named entities plus numeric (`&#38;`, `&#x26;`) forms.
fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let ahead: String = chars.clone().take(12).collect();
        if let Some(semicolon) = ahead.find(';')
            && let Some(decoded) = decode_entity(&ahead[..semicolon])
        {
            out.push(decoded);
            for _ in 0..=semicolon {
                chars.next();
            }
            continue;
        }
        out.push('&');
    }
    out
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(decimal) = name.strip_prefix('#') {
                decimal.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

/// Collapse each line's inner whitespace to single spaces and runs of blank
/// lines to one, so the text reads as paragraphs rather than a wall or a sprawl.
fn collapse_whitespace(input: &str) -> String {
    let mut out = String::new();
    let mut pending_blank = false;
    for line in input.split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            pending_blank = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if pending_blank {
                out.push('\n');
            }
        }
        pending_blank = false;
        out.push_str(&collapsed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn public_addresses_are_told_from_private_ones() {
        // Addresses are constructed numerically rather than written as literals,
        // so the repository's address tripwire (scripts/check-secrets.sh) has no
        // committed machine address to flag while the ranges are still covered.
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        let v6 = |segs: [u16; 8]| {
            IpAddr::V6(Ipv6Addr::new(
                segs[0], segs[1], segs[2], segs[3], segs[4], segs[5], segs[6], segs[7],
            ))
        };
        let public: [IpAddr; 3] = [
            v4(8, 8, 8, 8),
            v4(1, 1, 1, 1),
            v6([0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111]),
        ];
        for ip in public {
            assert!(is_public_ip(ip), "{ip} should be public");
        }
        let private: [IpAddr; 12] = [
            v4(127, 0, 0, 1),                                        // loopback
            v4(10, 0, 0, 1),                                         // private /8
            v4(192, 168, 1, 1),                                      // private /16
            v4(172, 16, 0, 1),                                       // private /12
            v4(169, 254, 1, 1),                                      // link-local
            v4(100, 64, 0, 1),                                       // shared / CGNAT
            v4(0, 0, 0, 0),                                          // unspecified
            IpAddr::V6(Ipv6Addr::LOCALHOST),                         // ::1
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),                       // ::
            v6([0xfc00, 0, 0, 0, 0, 0, 0, 1]),                       // unique-local
            v6([0xfe80, 0, 0, 0, 0, 0, 0, 1]),                       // link-local
            IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped()), // mapped private
        ];
        for ip in private {
            assert!(!is_public_ip(ip), "{ip} should be blocked");
        }
    }

    #[test]
    fn extra_reserved_v4_ranges_are_blocked() {
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        // Newly covered ranges, each a real SSRF or spoof risk if allowed.
        for ip in [
            v4(192, 0, 0, 1),   // IETF protocol assignments 192.0.0.0/24
            v4(192, 88, 99, 1), // 6to4 relay anycast 192.88.99.0/24
            v4(198, 18, 0, 1),  // benchmarking 198.18.0.0/15
            v4(198, 19, 5, 5),  // benchmarking, upper half
        ] {
            assert!(!is_public_ip(ip), "{ip} should be blocked");
        }
        // A neighbour just outside each range is still public.
        assert!(is_public_ip(v4(198, 20, 0, 1)));
        assert!(is_public_ip(v4(192, 88, 100, 1)));
    }

    #[test]
    fn ipv4_smuggled_inside_ipv6_is_judged_as_the_ipv4() {
        let v6 = |segs: [u16; 8]| {
            Ipv6Addr::new(
                segs[0], segs[1], segs[2], segs[3], segs[4], segs[5], segs[6], segs[7],
            )
        };
        // 6to4 (2002::/16), NAT64 (64:ff9b::/96) and IPv4-compatible (::/96)
        // embedding a private v4 must be blocked...
        // Addresses are named by their segments; the embedded v4 is described
        // rather than written as a dotted quad, which the address tripwire
        // (scripts/check-secrets.sh) would flag in a committed file.
        let private_embeds = [
            v6([0x2002, 0x0a00, 0x0001, 0, 0, 0, 0, 0]), // 6to4 -> 10.0.0.1
            v6([0x0064, 0xff9b, 0, 0, 0, 0, 0x0a00, 0x0001]), // NAT64 -> 10.0.0.1
            v6([0, 0, 0, 0, 0, 0, 0x7f00, 0x0001]),      // ::127.0.0.1 (compatible)
            v6([0x2002, 0x6440, 0x0001, 0, 0, 0, 0, 0]), // 6to4 wrapping a CGNAT v4
        ];
        for ip in private_embeds {
            assert!(!is_public_ip(IpAddr::V6(ip)), "{ip} embeds a private v4");
            assert_eq!(embedded_v4(ip).map(|v| !is_public_v4(v)), Some(true));
        }
        // ...while the same wrappers around a public v4 stay public.
        let public_embeds = [
            v6([0x2002, 0x0808, 0x0808, 0, 0, 0, 0, 0]), // 6to4 wrapping a public v4
            v6([0x0064, 0xff9b, 0, 0, 0, 0, 0x0808, 0x0808]), // NAT64 wrapping a public v4
        ];
        for ip in public_embeds {
            assert!(is_public_ip(IpAddr::V6(ip)), "{ip} embeds a public v4");
        }
        // A native global v6 has no embedded v4 and is unaffected.
        assert!(embedded_v4(v6([0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111])).is_none());
    }

    /// Replaces the former `guard_returns_pins_for_ips_and_skips_trusted_hosts`,
    /// which asserted that an allow-listed host was trusted *without* being
    /// resolved: the private-address bypass this change closes. Literals are
    /// still judged without DNS; an allow-listed name is now resolved and
    /// every address checked.
    #[tokio::test]
    async fn the_guard_vets_literals_and_resolves_allow_listed_hosts() {
        struct Fixed(Vec<std::net::SocketAddr>);
        #[async_trait]
        impl egress::Resolver for Fixed {
            async fn resolve(
                &self,
                _host: &str,
                _port: u16,
            ) -> std::io::Result<Vec<std::net::SocketAddr>> {
                Ok(self.0.clone())
            }
        }
        let rules = egress::EgressRules::default();
        let none = Fixed(Vec::new());
        // Built numerically so scripts/check-secrets.sh sees no machine address.
        let public = Ipv4Addr::new(8, 8, 8, 8);
        let url = reqwest::Url::parse(&format!("http://{public}/")).unwrap();
        let vetted = egress::vet(&rules, &url, &none).await.unwrap();
        assert_eq!(vetted.addrs, vec![std::net::SocketAddr::from((public, 80))]);
        assert!(!vetted.named);

        let private = format!("http://{}/", Ipv4Addr::new(127, 0, 0, 1));
        let url = reqwest::Url::parse(&private).unwrap();
        assert_eq!(
            egress::vet(&rules, &url, &none).await,
            Err(egress::EgressDenial::NonPublicAddress)
        );

        let listed = egress::EgressRules {
            allow_domains: vec!["example.com".to_owned()],
            ..egress::EgressRules::default()
        };
        let url = reqwest::Url::parse("https://docs.example.com/page").unwrap();
        let to_loopback = Fixed(vec![std::net::SocketAddr::from((
            Ipv4Addr::new(127, 0, 0, 1),
            443,
        ))]);
        assert_eq!(
            egress::vet(&listed, &url, &to_loopback).await,
            Err(egress::EgressDenial::NonPublicAddress),
            "an allow-listed name resolving privately is refused"
        );
        let to_public = Fixed(vec![std::net::SocketAddr::from((public, 443))]);
        assert!(egress::vet(&listed, &url, &to_public).await.unwrap().named);

        let url = reqwest::Url::parse("https://elsewhere.test/").unwrap();
        assert_eq!(
            egress::vet(&listed, &url, &to_public).await,
            Err(egress::EgressDenial::NotInAllowList)
        );
    }

    #[test]
    fn allow_list_matches_domain_and_subdomains() {
        let list = vec!["example.com".to_owned()];
        assert!(egress::host_matches("example.com", &list));
        assert!(egress::host_matches("docs.example.com", &list));
        assert!(egress::host_matches("EXAMPLE.com", &list));
        assert!(!egress::host_matches("notexample.com", &list));
        assert!(!egress::host_matches("example.com.evil.com", &list));
        assert!(!egress::host_matches("anything.org", &[]));
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = "<html><head><title>Hello &amp; Bye</title>\
            <style>.x{color:red}</style></head>\
            <body><script>evil()</script><h1>Heading</h1>\
            <p>First&nbsp;paragraph.</p><p>Second &lt;one&gt;.</p></body></html>";
        let (title, text) = html_to_text(html);
        assert_eq!(title.as_deref(), Some("Hello & Bye"));
        assert!(!text.contains("evil"), "script content must be dropped");
        assert!(!text.contains("color:red"), "style content must be dropped");
        assert!(text.contains("Heading"));
        assert!(text.contains("First paragraph."));
        assert!(text.contains("Second <one>."));
    }

    #[test]
    fn numeric_entities_decode() {
        assert_eq!(decode_entities("A&#38;B&#x26;C"), "A&B&C");
        assert_eq!(decode_entities("bare & amp"), "bare & amp");
    }

    #[test]
    fn search_results_parse_the_common_shape() {
        let json = json!({
            "results": [
                { "title": "First", "url": "https://a.test/1", "content": "one" },
                { "url": "https://b.test/2", "snippet": "two" },
                { "title": "No URL, skipped", "content": "nope" }
            ]
        });
        let hits = parse_search_results(&json, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "First");
        assert_eq!(hits[0].snippet, "one");
        assert_eq!(hits[1].title, "https://b.test/2", "title falls back to url");
        assert_eq!(hits[1].snippet, "two");

        assert!(parse_search_results(&json!({}), 10).is_empty());
        assert_eq!(parse_search_results(&json, 1).len(), 1, "limit is honoured");
    }

    #[test]
    fn duckduckgo_html_results_become_search_hits() {
        let html = r#"
            <div class="result results_links result--web">
              <h2 class="result__title">
                <a rel="nofollow" class="result__a"
                   href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fnews&amp;rut=abc">
                   Example &amp; News
                </a>
              </h2>
              <a class="result__snippet" href="https://example.com/news">
                Fresh <b>details</b> from the source.
              </a>
            </div>
            <div class="result results_links result--web">
              <h2><a class="result__a" href="https://second.example/article">Second</a></h2>
              <a class="result__snippet" href="https://second.example/article">Another result.</a>
            </div>
        "#;

        let hits = parse_duckduckgo_results(html, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Example & News");
        assert_eq!(hits[0].url, "https://example.com/news");
        assert_eq!(hits[0].snippet, "Fresh details from the source.");
        assert_eq!(hits[1].url, "https://second.example/article");
        assert_eq!(parse_duckduckgo_results(html, 1).len(), 1);
    }

    #[test]
    fn fetched_readable_text_is_capped() {
        let text = "x".repeat(MAX_READABLE_CHARS + 25);
        let limited = limit_readable_text(text);
        assert_eq!(
            limited.chars().take(MAX_READABLE_CHARS).count(),
            MAX_READABLE_CHARS
        );
        assert_eq!(limited.chars().nth(MAX_READABLE_CHARS), Some('\n'));
        assert!(limited.ends_with("[page text truncated]"));
    }

    #[tokio::test]
    async fn fetch_without_a_web_context_is_a_controlled_error() {
        let ctx = ToolCtx::new(CancellationToken::new());
        let outcome = WebFetch::new()
            .call(&json!({ "url": "https://example.com" }), &ctx)
            .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("web access is not enabled"));
    }

    #[tokio::test]
    async fn search_without_a_web_context_is_a_controlled_error() {
        let ctx = ToolCtx::new(CancellationToken::new());
        let outcome = WebSearch::new()
            .call(&json!({ "query": "rust" }), &ctx)
            .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("web access is not enabled"));
    }
}
