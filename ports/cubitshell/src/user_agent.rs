//! Penny's User-Agent presentation (docs/servo-port.md, "User-Agent").
//!
//! Penny presents its own User-Agent, the CuBit form ([`CUBIT_USER_AGENT`];
//! the presentation [`Presentation::Penny`]). For compatibility a user may
//! choose another platform's Firefox instead, for every site or per host. The
//! choice is typed ([`Presentation`]), never free text: each presentation is
//! a fixed User-Agent and `navigator.platform`, the named constants below.
//! Servo reads the result from one table (`servo::agent_identity`, patched
//! in by userspace/servo/agent_identity.py) for both the HTTP header and
//! `navigator.userAgent`, so the two agree.
//!
//! Where the choice comes from:
//! - CuBit: the typed settings `browser.servo.user-agent` and the family
//!   `browser.servo.site-agent.<host>` (CCL.Interfaces.Browser_Agent), read
//!   by the native shell (Servo_Agent_Settings, Servo_Site_Agent_Settings).
//! - Hosted (the Linux measurement harness only): the flag files
//!   `user-agent` (a presentation name) and `site-agents` (lines of
//!   "host name"), in PENNY_FLAGS.
//!
//! On CuBit the flag files, when present, take precedence (diagnostics:
//! the WEB-001 survey's `--flag user-agent=windows`).

use servo::agent_identity::{Identity, SiteIdentity};

/// The Firefox release every presentation names: the one Servo's own user
/// agent claims (components/config/prefs.rs). Change them together.
macro_rules! firefox_release {
    () => {
        "153.0"
    };
}

/// Penny's own: CuBit, Servo, and the Firefox token that keeps the
/// Gecko-compatible content sites serve Servo.
pub const CUBIT_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (CuBit; x86_64; rv:",
    firefox_release!(),
    ") Servo/",
    env!("CARGO_PKG_VERSION"),
    " Firefox/",
    firefox_release!()
);
/// Firefox on 64-bit Linux, exactly as Firefox sends it.
pub const LINUX_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (X11; Linux x86_64; rv:",
    firefox_release!(),
    ") Gecko/20100101 Firefox/",
    firefox_release!()
);
/// Firefox on 64-bit Windows 10 and 11 (both send NT 10.0).
pub const WINDOWS_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:",
    firefox_release!(),
    ") Gecko/20100101 Firefox/",
    firefox_release!()
);
/// Firefox on macOS (Firefox reports 10.15 on every later macOS too).
pub const MAC_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:",
    firefox_release!(),
    ") Gecko/20100101 Firefox/",
    firefox_release!()
);
/// `navigator.platform` for each, as that browser reports it.
pub const CUBIT_PLATFORM: &str = "CuBit";
pub const LINUX_PLATFORM: &str = "Linux x86_64";
pub const WINDOWS_PLATFORM: &str = "Win32";
pub const MAC_PLATFORM: &str = "MacIntel";

/// The presentations, in the order of the CCL enumeration
/// Agent_Presentation (the native shell passes its position).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presentation {
    /// Penny's own: CuBit's User-Agent and platform.
    #[default]
    Penny,
    Linux,
    Windows,
    Mac,
}

impl Presentation {
    pub const ALL: [Presentation; 4] = [Self::Penny, Self::Linux, Self::Windows, Self::Mac];

    /// The name a flag file uses.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Penny => "penny",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Mac => "mac",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The position in Agent_Presentation.
    pub fn from_position(position: u32) -> Option<Self> {
        Self::ALL.get(usize::try_from(position).ok()?).copied()
    }

    pub const fn user_agent(self) -> &'static str {
        match self {
            Self::Penny => CUBIT_USER_AGENT,
            Self::Linux => LINUX_USER_AGENT,
            Self::Windows => WINDOWS_USER_AGENT,
            Self::Mac => MAC_USER_AGENT,
        }
    }

    pub const fn platform(self) -> &'static str {
        match self {
            Self::Penny => CUBIT_PLATFORM,
            Self::Linux => LINUX_PLATFORM,
            Self::Windows => WINDOWS_PLATFORM,
            Self::Mac => MAC_PLATFORM,
        }
    }

    pub fn identity(self) -> Identity {
        Identity { user_agent: self.user_agent().into(), platform: self.platform().into() }
    }
}

/// A host's presentation: the host (lower case, no trailing dot) covers its
/// subdomains in Servo's table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub host: String,
    pub presentation: Presentation,
}

/// Every site's presentation and the per-host ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub global: Presentation,
    pub sites: Vec<Site>,
}

/// A host name or address as a table key, or why not: lower case letters,
/// digits, '-' and '.', or a bracketed IPv6 address; no empty label.
pub fn host_key(host: &str) -> Result<String, String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let address = host.starts_with('[') && host.ends_with(']') &&
        host[1..host.len() - 1].parse::<std::net::Ipv6Addr>().is_ok();
    let name = !host.is_empty() &&
        host.split('.').all(|label| !label.is_empty()) &&
        host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.');
    if address || name { Ok(host) } else { Err(format!("not a host name: {host:?}")) }
}

impl Choice {
    /// The `site-agents` flag file: one "host presentation" per line.
    pub fn parse_sites(text: &str) -> Result<Vec<Site>, String> {
        let mut sites = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(host), Some(name), None) = (fields.next(), fields.next(), fields.next()) else {
                return Err(format!("site-agents line {}: want \"host presentation\"", number + 1));
            };
            let presentation = Presentation::from_name(name)
                .ok_or_else(|| format!("site-agents line {}: no presentation {name:?}", number + 1))?;
            sites.push(Site { host: host_key(host)?, presentation });
        }
        Ok(sites)
    }

    /// Servo's table: the default identity and the per-host ones.
    pub fn install(&self) {
        servo::agent_identity::set(
            self.global.identity(),
            self.sites
                .iter()
                .map(|site| SiteIdentity { host: site.host.clone(), identity: site.presentation.identity() })
                .collect(),
        );
    }
}

/// The choice in force: the flag files where present, else (on CuBit) the
/// settings, else Penny's own everywhere. A flag file that does not parse is an
/// error, said and ignored (never a free-text User-Agent).
pub fn load(say: impl Fn(&str)) -> Choice {
    #[cfg(target_os = "cubit")]
    let mut choice = crate::cubit_desktop::user_agent_choice();
    #[cfg(not(target_os = "cubit"))]
    let mut choice = Choice::default();
    if let Ok(text) = std::fs::read_to_string(crate::flag_path("user-agent")) {
        match Presentation::from_name(text.trim()) {
            Some(presentation) => choice.global = presentation,
            None => say(&format!("CUBITSHELL: user-agent flag: no presentation {:?} (penny, linux, windows, mac)", text.trim())),
        }
    }
    if let Ok(text) = std::fs::read_to_string(crate::flag_path("site-agents")) {
        match Choice::parse_sites(&text) {
            Ok(sites) => choice.sites = sites,
            Err(error) => say(&format!("CUBITSHELL: {error}")),
        }
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_penny() {
        assert_eq!(Choice::default().global, Presentation::Penny);
        assert!(CUBIT_USER_AGENT.starts_with("Mozilla/5.0 (CuBit; x86_64; rv:"));
    }

    #[test]
    fn names_round_trip() {
        for presentation in Presentation::ALL {
            assert_eq!(Presentation::from_name(presentation.name()), Some(presentation));
        }
        assert_eq!(Presentation::from_position(2), Some(Presentation::Windows));
        assert_eq!(Presentation::from_position(4), None);
    }

    #[test]
    fn sites_parse() {
        let sites = Choice::parse_sites("Example.COM. windows\n\n[::1] mac\n").unwrap();
        assert_eq!(sites[0], Site { host: "example.com".into(), presentation: Presentation::Windows });
        assert_eq!(sites[1].host, "[::1]");
        assert!(Choice::parse_sites("example.com Mozilla/5.0").is_err());
        assert!(Choice::parse_sites("exa mple.com windows").is_err());
        assert!(Choice::parse_sites("a..b windows").is_err());
    }
}
