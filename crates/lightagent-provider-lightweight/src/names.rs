//! Provider-compatible tool names.
//!
//! Lightagent's tool names are namespaced with dots (`web.search`,
//! `mcp.server.tool`). OpenAI's Chat Completions API and providers that copy
//! its validation accept only `^[a-zA-Z0-9_-]{1,64}$` for a function name;
//! Anthropic's own API allows the same characters up to 128, and Gemini's
//! function declarations the same set up to 64. A dotted name makes such an
//! endpoint refuse the whole request.
//!
//! With `inference.tool_names = "portable"` every name crosses the wire
//! through one [`ToolNameMap`], built per request from every name that
//! request carries (declared tools and the tool calls in its history):
//!
//! * a name already in the portable alphabet and within 64 characters is
//!   sent unchanged;
//! * otherwise `.` becomes `__` and any other character `_`;
//! * a name that would then collide with another, or is too long, gets a
//!   short hash of its internal name appended (within 64 characters);
//! * a map that would still hold a collision is refused before anything is
//!   sent, rather than guessed.
//!
//! Inbound, a returned name is looked up in the same map. A name the map
//! does not hold is never matched loosely: it becomes an `undeclared:` name
//! no tool has, so the executor refuses the call as unavailable. The
//! internal name is what the executor, the approval prompt, the policy guard
//! and the audit trail see; the provider-facing name never leaves this crate.
//!
//! `native` (the default) sends the internal names unchanged, as before.

use std::collections::BTreeMap;

use lightagent_core::provider::ProviderError;

/// The longest portable function name.
pub const MAX_PORTABLE_NAME: usize = 64;
/// What a returned name that was never declared becomes.
pub const UNDECLARED_PREFIX: &str = "undeclared:";

fn portable_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Whether `name` is already acceptable to a strict provider.
pub fn is_portable(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_PORTABLE_NAME && name.chars().all(portable_char)
}

/// The readable portable form of `name`: `.` as `__`, anything else outside
/// the alphabet as `_`. Not yet checked for length or collisions.
fn readable(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        match c {
            '.' => out.push_str("__"),
            c if portable_char(c) => out.push(c),
            _ => out.push('_'),
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// A short, stable hash of `name` (FNV-1a, 32 bits), to tell names apart.
fn short_hash(name: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// `base` with a hash of `internal` appended, within the length limit.
fn hashed(base: &str, internal: &str) -> String {
    let suffix = format!("_{}", short_hash(internal));
    let room = MAX_PORTABLE_NAME - suffix.len();
    let base: String = base.chars().take(room).collect();
    format!("{base}{suffix}")
}

/// One request's two-way mapping between internal and provider names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolNameMap {
    outbound: BTreeMap<String, String>,
    inbound: BTreeMap<String, String>,
}

impl ToolNameMap {
    /// The portable map of `names`, or an error when two names cannot be
    /// told apart.
    pub fn portable<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Self, ProviderError> {
        let mut internal: Vec<&str> = names.into_iter().collect();
        internal.sort_unstable();
        internal.dedup();
        // First choice for each name, then who else wants the same one.
        let first: Vec<(&str, String)> = internal
            .iter()
            .map(|name| {
                let candidate = if is_portable(name) {
                    (*name).to_owned()
                } else {
                    readable(name)
                };
                (*name, candidate)
            })
            .collect();
        let mut claims: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, candidate) in &first {
            *claims.entry(candidate.as_str()).or_default() += 1;
        }
        let mut map = Self::default();
        for (name, candidate) in &first {
            let contested = claims.get(candidate.as_str()).copied().unwrap_or(0) > 1;
            let chosen = if contested || candidate.len() > MAX_PORTABLE_NAME {
                hashed(candidate, name)
            } else {
                candidate.clone()
            };
            if map.inbound.contains_key(&chosen) {
                return Err(ProviderError::Upstream(format!(
                    "two tools cannot be given distinct provider-compatible names ({chosen}); \
                     rename one, or set inference.tool_names to native"
                )));
            }
            map.inbound.insert(chosen.clone(), (*name).to_owned());
            map.outbound.insert((*name).to_owned(), chosen);
        }
        Ok(map)
    }

    /// The provider name of an internal name. A name outside the map (which
    /// a request never sends, since the map is built from all of them) still
    /// gets a portable spelling rather than a refused request.
    pub fn outbound(&self, internal: &str) -> String {
        self.outbound
            .get(internal)
            .cloned()
            .unwrap_or_else(|| hashed(&readable(internal), internal))
    }

    /// The internal name of a returned provider name, or an `undeclared:`
    /// name that no tool has.
    pub fn inbound(&self, provider: &str) -> String {
        match self.inbound.get(provider) {
            Some(internal) => internal.clone(),
            None => format!("{UNDECLARED_PREFIX}{provider}"),
        }
    }

    /// How many names the map holds.
    pub fn len(&self) -> usize {
        self.outbound.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.outbound.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(name: &str) -> bool {
        is_portable(name)
    }

    #[test]
    fn dotted_names_become_portable_and_come_back() {
        let map = ToolNameMap::portable([
            "web.search",
            "web.fetch",
            "agent.delegate",
            "mcp.docs-server.find_page",
            "datetime_now",
        ])
        .unwrap();
        assert_eq!(map.outbound("web.search"), "web__search");
        assert_eq!(
            map.outbound("mcp.docs-server.find_page"),
            "mcp__docs-server__find_page"
        );
        // An already portable name is sent unchanged.
        assert_eq!(map.outbound("datetime_now"), "datetime_now");
        for internal in ["web.search", "web.fetch", "agent.delegate", "datetime_now"] {
            let sent = map.outbound(internal);
            assert!(valid(&sent), "{sent}");
            assert_eq!(map.inbound(&sent), internal);
        }
    }

    #[test]
    fn colliding_names_are_told_apart_by_hash() {
        // `a.b` reads as `a__b`, which is also a real tool's name; and `x y`
        // and `x?y` both read as `x_y`.
        let map = ToolNameMap::portable(["a.b", "a__b", "x y", "x?y"]).unwrap();
        let sent: Vec<String> = ["a.b", "a__b", "x y", "x?y"]
            .iter()
            .map(|name| map.outbound(name))
            .collect();
        let mut distinct = sent.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), 4, "{sent:?}");
        for (internal, provider) in ["a.b", "a__b", "x y", "x?y"].iter().zip(&sent) {
            assert!(valid(provider), "{provider}");
            assert_eq!(&map.inbound(provider), internal);
        }
        // The contested names keep a readable prefix.
        assert!(sent[0].starts_with("a__b_"), "{sent:?}");
    }

    #[test]
    fn long_and_empty_names_fit_the_limit() {
        let long = format!("mcp.{}.tool", "s".repeat(80));
        let map = ToolNameMap::portable([long.as_str(), ""]).unwrap();
        let sent = map.outbound(&long);
        assert_eq!(sent.len(), MAX_PORTABLE_NAME);
        assert!(valid(&sent));
        assert_eq!(map.inbound(&sent), long);
        let empty = map.outbound("");
        assert!(valid(&empty), "{empty:?}");
        assert_eq!(map.inbound(&empty), "");
    }

    #[test]
    fn unknown_and_internal_spellings_are_never_matched() {
        let map = ToolNameMap::portable(["web.search"]).unwrap();
        // The model answering with the internal spelling, a near miss or an
        // invented name: none resolves to a tool.
        for returned in ["web.search", "web_search", "WEB__SEARCH", "fs__write"] {
            let name = map.inbound(returned);
            assert!(name.starts_with(UNDECLARED_PREFIX), "{returned} -> {name}");
            assert_ne!(name, "web.search");
        }
    }
}
