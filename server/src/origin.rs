//! Which `Host` and `Origin` headers the server answers.
//!
//! Against DNS rebinding: a page on the internet can point a name of its own
//! at the Pi's LAN address and then read the API as if it were that site. A
//! name like that is never an IP literal, a single label, a local-only name
//! or this machine's own name on the network ([`Own`]), so those are let
//! through with no setup; anything else needs `HELIX_ALLOWED_HOSTS`.
//!
//! Against cross-site WebSockets: CORS does not cover them, so a page on any
//! site could read `/api/live`. Browsers send `Origin` with every WebSocket
//! (and every write), and it must name this server.

use std::net::IpAddr;

/// Suffixes no public DNS answers for (mDNS, RFC 6761/8375, ICANN's
/// `.internal`, and the common home-router ones).
const LOCAL_SUFFIXES: [&str; 6] = [
    ".local",
    ".localhost",
    ".internal",
    ".home.arpa",
    ".lan",
    ".home",
];

/// A name as the server compares it: lower case, no trailing dot.
pub fn normalise(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// `host[:port]` or `[v6][:port]`, as in `Host` and after `scheme://` in
/// `Origin`. `None` if malformed.
fn split(authority: &str) -> Option<(String, Option<u16>)> {
    let (name, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (name, after) = rest.split_once(']')?;
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':')?),
            };
            (name, port)
        }
        None => match authority.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (authority, None),
        },
    };
    let port = port.map(str::parse::<u16>).transpose().ok()?;
    let name = normalise(name);
    (!name.is_empty()).then_some((name, port))
}

/// Whether a `Host` header names something only the LAN can resolve, or a
/// name in `extra` (already [`normalise`]d).
pub fn host_allowed(host: &str, extra: &[String]) -> bool {
    let Some((name, _)) = split(host) else {
        return false;
    };
    name.parse::<IpAddr>().is_ok()
        || !name.contains('.')
        || LOCAL_SUFFIXES.iter().any(|s| name.ends_with(s))
        || extra.contains(&name)
}

/// This machine's names: its hostname, and the domains the network's DHCP
/// gave it, so `helix-bridge.fritz.box` (the router's DNS name for the Pi)
/// works with no setup. Only the exact `<hostname>.<domain>`: another name
/// under the router's domain is still refused.
#[derive(Debug, Clone, PartialEq)]
pub struct Own {
    /// Hostname without any domain, e.g. `helix-bridge`.
    pub host: String,
    /// `search`/`domain` entries of `/etc/resolv.conf`, e.g. `fritz.box`.
    pub domains: Vec<String>,
}

impl Own {
    /// Read now, not at start-up: a Pi taken from home to a gig gets another
    /// domain from that network's router, and needs no restart for it.
    /// `None` when the hostname is unknown.
    pub fn read() -> Option<Own> {
        let host = std::fs::read_to_string("/etc/hostname")
            .ok()
            .or_else(|| std::env::var("COMPUTERNAME").ok())?;
        let resolv = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
        Own::new(&host, &resolv)
    }

    fn new(hostname: &str, resolv: &str) -> Option<Own> {
        let host = normalise(hostname).split('.').next()?.to_owned();
        let domains = resolv
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                matches!(words.next(), Some("search" | "domain")).then_some(words)
            })
            .flatten()
            .map(normalise)
            .filter(|d| !d.is_empty())
            .collect();
        (!host.is_empty()).then_some(Own { host, domains })
    }

    /// Whether a `Host` header is one of this machine's names.
    pub fn answers(&self, host: &str) -> bool {
        let Some((name, _)) = split(host) else {
            return false;
        };
        name.strip_prefix(&self.host)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|domain| self.domains.iter().any(|d| d == domain))
    }

    /// `http://helix-bridge.local/`, on the port `host` used.
    pub fn local_url(&self, host: &str) -> String {
        let port = split(host)
            .and_then(|(_, port)| port)
            .filter(|&p| p != 80)
            .map(|p| format!(":{p}"))
            .unwrap_or_default();
        format!("http://{}.local{port}/", self.host)
    }
}

/// Whether `Origin` is the server the request came to: same name, same
/// port (a missing one is the scheme's default).
pub fn same_origin(origin: &str, host: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return false,
    };
    match (split(authority), split(host)) {
        (Some((a, a_port)), Some((b, b_port))) => {
            a == b && a_port.unwrap_or(default) == b_port.unwrap_or(default)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_pass() {
        for host in [
            "helix-bridge.local",
            "Helix-Bridge.LOCAL.",
            "helix-bridge.local:80",
            "helix-bridge",
            "localhost:8080",
            "pi.home.arpa",
            "pi.lan",
            "192.168.1.20",
            "192.168.1.20:8080",
            "[fe80::1]",
            "[::1]:8080",
        ] {
            assert!(host_allowed(host, &[]), "{host}");
        }
    }

    #[test]
    fn internet_names_fail() {
        for host in [
            "evil.example",
            "evil.example:80",
            "helix-bridge.local.evil.example",
            "192.168.1.20.nip.io",
            "",
            ":80",
            "[::1",
            "[::1]x",
            "pi.local:http",
        ] {
            assert!(!host_allowed(host, &[]), "{host}");
        }
    }

    #[test]
    fn extra_names() {
        let extra = [normalise("Pi.Fritz.Box.")];
        assert!(host_allowed("pi.fritz.box:80", &extra));
        assert!(host_allowed("PI.FRITZ.BOX", &extra));
        assert!(!host_allowed("other.fritz.box", &extra));
    }

    #[test]
    fn own_names() {
        let resolv = "# Generated by NetworkManager\nsearch fritz.box Home.\n\
                      nameserver 192.168.178.1\ndomain lan\nsearch .\n";
        let own = Own::new("Helix-Bridge\n", resolv).unwrap();
        assert_eq!(own.host, "helix-bridge");
        assert_eq!(own.domains, ["fritz.box", "home", "lan"]);
        for host in [
            "helix-bridge.fritz.box",
            "HELIX-BRIDGE.fritz.box.:8080",
            "helix-bridge.lan",
        ] {
            assert!(own.answers(host), "{host}");
        }
        for host in [
            "nas.fritz.box",
            "helix-bridge.evil.example",
            "helix-bridgex.fritz.box",
            "helix-bridge",
            "fritz.box",
        ] {
            assert!(!own.answers(host), "{host}");
        }
        // A full name in /etc/hostname, and no resolv.conf.
        let own = Own::new("helix-bridge.example.org", "").unwrap();
        assert_eq!((own.host.as_str(), own.domains.len()), ("helix-bridge", 0));
        assert!(Own::new(" \n", "").is_none());
    }

    #[test]
    fn local_url() {
        let own = Own::new("helix-bridge", "").unwrap();
        assert_eq!(own.local_url("evil.example"), "http://helix-bridge.local/");
        assert_eq!(
            own.local_url("evil.example:80"),
            "http://helix-bridge.local/"
        );
        assert_eq!(
            own.local_url("evil.example:8080"),
            "http://helix-bridge.local:8080/"
        );
    }

    #[test]
    fn origins() {
        assert!(same_origin(
            "http://helix-bridge.local",
            "helix-bridge.local"
        ));
        assert!(same_origin(
            "http://helix-bridge.local",
            "helix-bridge.local:80"
        ));
        assert!(same_origin(
            "HTTP://Helix-Bridge.local:8080",
            "helix-bridge.local:8080"
        ));
        assert!(same_origin("https://pi.lan", "pi.lan:443"));
        assert!(same_origin("http://[::1]:8080", "[::1]:8080"));

        assert!(!same_origin("http://evil.example", "helix-bridge.local"));
        assert!(!same_origin(
            "http://helix-bridge.local:8080",
            "helix-bridge.local"
        ));
        assert!(!same_origin("null", "helix-bridge.local"));
        assert!(!same_origin("file://", "helix-bridge.local"));
    }
}
