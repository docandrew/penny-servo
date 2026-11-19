/* CuBit: Penny's User-Agent presentation (agent_identity.py). */

//! Who the browser says it is: the `User-Agent` header, `navigator.userAgent`
//! and `navigator.platform`, chosen by the embedder per host (cubitshell's
//! typed User-Agent setting). One table, read by net for the header and by
//! script for the navigator, so the two always agree. A host's entry covers
//! its subdomains (as Firefox's per-domain overrides do) and the most
//! specific entry wins; any other host, and a document without a host,
//! gets the default. Without a table (an embedder that sets none) the
//! `user_agent` preference and the build's platform apply, as before.
//!
//! The table is process-global, like the preferences: Penny runs Servo in
//! one process.

use std::net::IpAddr;
use std::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub user_agent: String,
    pub platform: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteIdentity {
    /// A host name or address, lower case, without a trailing dot.
    pub host: String,
    pub identity: Identity,
}

struct Table {
    default: Option<Identity>,
    sites: Vec<SiteIdentity>,
}

static TABLE: RwLock<Table> = RwLock::new(Table {
    default: None,
    sites: Vec::new(),
});

/// Install the default identity and the per-host ones.
pub fn set(default: Identity, sites: Vec<SiteIdentity>) {
    *TABLE.write().unwrap() = Table {
        default: Some(default),
        sites,
    };
}

/// Whether `site`'s entry covers `host`: the same host, or (for a name, not
/// an address) one of its subdomains.
fn covers(site: &str, host: &str) -> bool {
    if host == site {
        return true;
    }
    if host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>().is_ok() {
        return false;
    }
    host.len() > site.len() &&
        host.ends_with(site) &&
        host.as_bytes()[host.len() - site.len() - 1] == b'.'
}

/// The identity for a host (`None` when the embedder set no table).
pub fn for_host(host: Option<&str>) -> Option<Identity> {
    let table = TABLE.read().unwrap();
    let host = host.map(|host| host.trim_end_matches('.').to_ascii_lowercase());
    let site = host.as_deref().and_then(|host| {
        table
            .sites
            .iter()
            .filter(|site| covers(&site.host, host))
            .max_by_key(|site| site.host.len())
    });
    site.map(|site| site.identity.clone())
        .or_else(|| table.default.clone())
}

/// The User-Agent for a host: its identity's, else the preference's.
pub fn user_agent(host: Option<&str>) -> String {
    for_host(host)
        .map(|identity| identity.user_agent)
        .unwrap_or_else(|| crate::prefs::get().user_agent.clone())
}
