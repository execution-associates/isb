//! The egress policy: which hostnames and ports a sandbox may reach, and
//! which secrets may be put on the wire towards which hosts.
//!
//! `egress:` in a spec is one of `none`, a list of `host[:port]` entries, or
//! an object with `allow` and `secrets`. [`EgressSpec`] is that field as
//! written; [`Policy`] is the validated form the plumbing, the instance
//! config and the proxy all use.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// The port an entry gets when it names none.
pub const DEFAULT_PORT: u16 = 443;

/// A host the policy names: one exact hostname, or every name under a
/// suffix (`*.example.com`, not `example.com` itself).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostPattern {
    Exact(String),
    /// The domain after `*.`.
    Suffix(String),
}

impl HostPattern {
    /// Whether `host` (already lower-cased, no trailing dot) is covered.
    pub fn matches(&self, host: &str) -> bool {
        match self {
            HostPattern::Exact(h) => h == host,
            HostPattern::Suffix(d) => host
                .strip_suffix(d.as_str())
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
        }
    }
}

/// One allowed `host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entry {
    pub host: HostPattern,
    pub port: u16,
}

/// A hostname lower-cased and checked: DNS labels only, never an IP address.
pub fn normalize_host(raw: &str) -> Result<String> {
    let h = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 {
        return Err(Error::invalid(format!(
            "egress host {raw:?}: not a hostname"
        )));
    }
    if h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[') {
        return Err(Error::invalid(format!(
            "egress host {raw:?}: egress is by hostname; an IP address cannot be allowed"
        )));
    }
    for label in h.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        if !ok {
            return Err(Error::invalid(format!(
                "egress host {raw:?}: {label:?} is not a valid DNS label"
            )));
        }
    }
    Ok(h)
}

impl FromStr for Entry {
    type Err = Error;

    /// `host`, `host:port`, `*.suffix` or `*.suffix:port`.
    fn from_str(s: &str) -> Result<Entry> {
        let s = s.trim();
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => {
                let port = p
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or_else(|| Error::invalid(format!("egress {s:?}: bad port {p:?}")))?;
                (h, port)
            }
            None => (s, DEFAULT_PORT),
        };
        let host = match host.strip_prefix("*.") {
            Some(domain) => {
                let d = normalize_host(domain)?;
                if !d.contains('.') {
                    return Err(Error::invalid(format!(
                        "egress {s:?}: a wildcard needs a domain of at least two labels"
                    )));
                }
                HostPattern::Suffix(d)
            }
            None if host.contains('*') => {
                return Err(Error::invalid(format!(
                    "egress {s:?}: `*` is only allowed as a leading `*.`"
                )));
            }
            None => HostPattern::Exact(normalize_host(host)?),
        };
        Ok(Entry { host, port })
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            HostPattern::Exact(h) => write!(f, "{h}:{}", self.port),
            HostPattern::Suffix(d) => write!(f, "*.{d}:{}", self.port),
        }
    }
}

impl Entry {
    /// Whether a connection to `host:port` is allowed by this entry.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        self.port == port && self.host.matches(host)
    }
}

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Entry, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A secret the sandbox sees only as a placeholder: the real value is put
/// on the wire towards `hosts` and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretBinding {
    /// The environment variable holding the placeholder inside the guest.
    pub env: String,
    /// The secret in the org's store.
    pub secret: String,
    /// Where the real value may be sent (TLS only).
    pub hosts: Vec<Entry>,
    /// What the guest sees. Derived from the sandbox and the variable.
    pub placeholder: String,
}

/// A secret as written in a spec: `{env, secret?, hosts}`, or the string
/// `ENV[=SECRET]@host1,host2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressSecretSpec {
    /// The environment variable the guest sees (holding a placeholder).
    pub env: String,
    /// The secret in the org's store (default: the variable's name).
    pub secret: Option<String>,
    /// Hosts the real value may be sent to: `host[:port]`, port 443 by default.
    pub hosts: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum SecretRepr {
    /// `ENV[=SECRET]@host1,host2`
    Short(String),
    /// A secret with its fields spelled out.
    Full {
        /// The environment variable the guest sees (holding a placeholder).
        env: String,
        /// The secret in the org's store (default: the variable's name).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
        /// Hosts the real value may be sent to: `host[:port]`, port 443 by default.
        hosts: Vec<String>,
    },
}

impl JsonSchema for EgressSecretSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EgressSecretSpec".into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        SecretRepr::json_schema(g)
    }
}

impl EgressSecretSpec {
    /// Parse `ENV[=SECRET]@host1,host2` (the `--secret` flag's form).
    pub fn parse(s: &str) -> Result<EgressSecretSpec> {
        let (head, hosts) = s.split_once('@').ok_or_else(|| {
            Error::invalid(format!(
                "egress secret {s:?}: expected NAME@host1,host2 (the hosts it may be sent to)"
            ))
        })?;
        let (env, secret) = match head.split_once('=') {
            Some((e, s)) => (e, Some(s.to_string())),
            None => (head, None),
        };
        Ok(EgressSecretSpec {
            env: env.to_string(),
            secret,
            hosts: hosts.split(',').map(|h| h.trim().to_string()).collect(),
        })
    }
}

impl Serialize for EgressSecretSpec {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        SecretRepr::Full {
            env: self.env.clone(),
            secret: self.secret.clone(),
            hosts: self.hosts.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for EgressSecretSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match SecretRepr::deserialize(d)? {
            SecretRepr::Short(s) => EgressSecretSpec::parse(&s).map_err(serde::de::Error::custom),
            SecretRepr::Full { env, secret, hosts } => Ok(EgressSecretSpec { env, secret, hosts }),
        }
    }
}

/// The `egress:` field as written in a spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressSpec {
    /// `host[:port]` entries (`*.example.com` for a suffix); port 443 by default.
    pub allow: Vec<String>,
    /// `egress: none`: no network at all. Also what an empty list means.
    pub none: bool,
    pub secrets: Vec<EgressSecretSpec>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum EgressRepr {
    /// `none`: no network at all.
    Keyword(String),
    /// The hosts the sandbox may reach: `host[:port]` (port 443 by default),
    /// `*.example.com` for subdomains.
    List(Vec<String>),
    /// Hosts and secrets.
    Full {
        /// The hosts the sandbox may reach: `host[:port]`.
        #[serde(default)]
        allow: Vec<String>,
        /// Secrets the guest sees only as placeholders.
        #[serde(default)]
        secrets: Vec<EgressSecretSpec>,
    },
}

impl JsonSchema for EgressSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EgressSpec".into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        EgressRepr::json_schema(g)
    }
}

impl Serialize for EgressSpec {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let repr = if self.none {
            EgressRepr::Keyword("none".into())
        } else if self.secrets.is_empty() {
            EgressRepr::List(self.allow.clone())
        } else {
            EgressRepr::Full {
                allow: self.allow.clone(),
                secrets: self.secrets.clone(),
            }
        };
        repr.serialize(s)
    }
}

impl<'de> Deserialize<'de> for EgressSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match EgressRepr::deserialize(d)? {
            EgressRepr::Keyword(k) if k.eq_ignore_ascii_case("none") => EgressSpec {
                none: true,
                ..Default::default()
            },
            EgressRepr::Keyword(k) => {
                return Err(serde::de::Error::custom(format!(
                    "egress: {k:?} is not `none`, a list of host[:port] or an object with allow/secrets"
                )));
            }
            EgressRepr::List(allow) => EgressSpec {
                allow,
                ..Default::default()
            },
            EgressRepr::Full { allow, secrets } => EgressSpec {
                allow,
                none: false,
                secrets,
            },
        })
    }
}

impl EgressSpec {
    /// `egress: none`.
    pub fn none() -> EgressSpec {
        EgressSpec {
            none: true,
            ..Default::default()
        }
    }

    /// Allow these `host[:port]` entries.
    pub fn allow<I, S>(hosts: I) -> EgressSpec
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        EgressSpec {
            allow: hosts.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }
}

/// The validated policy. An empty one denies everything (`egress: none`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Everything reachable: the allow list and every secret's hosts.
    pub entries: Vec<Entry>,
    pub secrets: Vec<SecretBinding>,
}

/// Most entries a sandbox may carry: the proxy and the DNS config are sized
/// by it.
pub const MAX_ENTRIES: usize = 256;
/// Most secrets a sandbox may carry.
pub const MAX_SECRETS: usize = 16;

fn valid_env_name(n: &str) -> bool {
    let mut b = n.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && n.len() <= 128
}

impl Policy {
    /// Validate a spec's `egress` for the sandbox whose egress network is
    /// `network` (which the placeholders are derived from).
    pub fn from_spec(spec: &EgressSpec, network: &str) -> Result<Policy> {
        if spec.none && (!spec.allow.is_empty() || !spec.secrets.is_empty()) {
            return Err(Error::invalid(
                "egress: none cannot be combined with hosts or secrets",
            ));
        }
        let mut entries = BTreeSet::new();
        for a in &spec.allow {
            entries.insert(a.parse::<Entry>()?);
        }
        let mut secrets: Vec<SecretBinding> = Vec::new();
        for s in &spec.secrets {
            if !valid_env_name(&s.env) {
                return Err(Error::invalid(format!(
                    "egress secret: {:?} is not an environment variable name",
                    s.env
                )));
            }
            if secrets.iter().any(|b| b.env == s.env) {
                return Err(Error::invalid(format!(
                    "egress secret {}: listed twice",
                    s.env
                )));
            }
            let store = s.secret.clone().unwrap_or_else(|| s.env.clone());
            crate::secrets::validate_name(&store)?;
            if s.hosts.is_empty() {
                return Err(Error::invalid(format!(
                    "egress secret {}: name the hosts it may be sent to (NAME@host1,host2)",
                    s.env
                )));
            }
            let hosts = s
                .hosts
                .iter()
                .map(|h| h.parse::<Entry>())
                .collect::<Result<Vec<_>>>()?;
            entries.extend(hosts.iter().cloned());
            secrets.push(SecretBinding {
                placeholder: placeholder(network, &s.env),
                env: s.env.clone(),
                secret: store,
                hosts,
            });
        }
        if secrets.len() > MAX_SECRETS || entries.len() > MAX_ENTRIES {
            return Err(Error::invalid(format!(
                "egress: at most {MAX_ENTRIES} hosts and {MAX_SECRETS} secrets per sandbox"
            )));
        }
        Ok(Policy {
            entries: entries.into_iter().collect(),
            secrets,
        })
    }

    /// Nothing is reachable: no network, no DNS.
    pub fn is_none(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `host:port` is on the list.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        self.entries.iter().any(|e| e.allows(host, port))
    }

    /// Every port the proxy has to listen on.
    pub fn ports(&self) -> BTreeSet<u16> {
        self.entries.iter().map(|e| e.port).collect()
    }

    /// The names DNS answers for, as dnsmasq understands them: a name
    /// covers itself and everything below it.
    pub fn dns_names(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .map(|e| match &e.host {
                HostPattern::Exact(h) => h.clone(),
                HostPattern::Suffix(d) => d.clone(),
            })
            .collect()
    }

    /// The secrets that may be put on the wire towards `host:port`.
    pub fn secrets_for<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Iterator<Item = &'a SecretBinding> {
        self.secrets
            .iter()
            .filter(move |s| s.hosts.iter().any(|e| e.allows(host, port)))
    }

    /// Whether a connection to `host:port` is terminated and rewritten (some
    /// secret is approved there) rather than passed through.
    pub fn intercepts(&self, host: &str, port: u16) -> bool {
        self.secrets_for(host, port).next().is_some()
    }

    /// For a protocol that shows no name (server-first, or not TLS and not
    /// HTTP): the one exact host that owns `port`, when there is exactly one.
    pub fn pinned_host(&self, port: u16) -> Option<&str> {
        let mut hosts = self.entries.iter().filter(|e| e.port == port);
        let first = hosts.next()?;
        let HostPattern::Exact(h) = &first.host else {
            return None;
        };
        hosts.next().is_none().then_some(h.as_str())
    }

    /// The `host:port` entries as strings, for display.
    pub fn list(&self) -> Vec<String> {
        self.entries.iter().map(Entry::to_string).collect()
    }
}

/// What the guest sees in place of a secret: stable for a sandbox and a
/// variable, so reconciling never changes it, and not derived from the value.
pub fn placeholder(network: &str, env: &str) -> String {
    let d = ring::digest::digest(
        &ring::digest::SHA256,
        format!("isb-egress-placeholder\0{network}\0{env}").as_bytes(),
    );
    let hex: String = d.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("isb_placeholder_{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str) -> Entry {
        s.parse().unwrap()
    }

    #[test]
    fn entries_parse_with_default_port() {
        assert_eq!(
            e("API.Example.com"),
            Entry {
                host: HostPattern::Exact("api.example.com".into()),
                port: 443
            }
        );
        assert_eq!(e("db.example.com:5432").port, 5432);
        assert_eq!(
            e("*.example.com:8443"),
            Entry {
                host: HostPattern::Suffix("example.com".into()),
                port: 8443
            }
        );
        assert_eq!(e("example.com.").to_string(), "example.com:443");
    }

    #[test]
    fn bad_entries_are_refused() {
        for bad in [
            "",
            "1.2.3.4",
            "1.2.3.4:443",
            "[::1]:443",
            "example.com:0",
            "example.com:70000",
            "example.com:x",
            "*.com",
            "*",
            "a*.example.com",
            "ex ample.com",
            "-bad.example.com",
            "a..b",
            "https://example.com",
        ] {
            assert!(bad.parse::<Entry>().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn exact_and_suffix_matching() {
        let exact = e("api.example.com");
        assert!(exact.allows("api.example.com", 443));
        assert!(!exact.allows("api.example.com", 80));
        assert!(!exact.allows("x.api.example.com", 443));
        assert!(!exact.allows("example.com", 443));
        let wild = e("*.example.com");
        assert!(wild.allows("a.example.com", 443));
        assert!(wild.allows("a.b.example.com", 443));
        assert!(!wild.allows("example.com", 443), "the apex is not covered");
        assert!(
            !wild.allows("badexample.com", 443),
            "a suffix is a label boundary"
        );
        assert!(!wild.allows("a.example.com.evil.com", 443));
        assert!(!wild.allows("a.example.com", 8443));
    }

    #[test]
    fn entries_roundtrip_as_strings() {
        let v = serde_json::to_string(&e("*.example.com:8443")).unwrap();
        assert_eq!(v, "\"*.example.com:8443\"");
        assert_eq!(
            serde_json::from_str::<Entry>(&v).unwrap(),
            e("*.example.com:8443")
        );
    }

    #[test]
    fn spec_forms_deserialize() {
        let none: EgressSpec = serde_yaml_ng::from_str("none").unwrap();
        assert!(none.none);
        let list: EgressSpec =
            serde_yaml_ng::from_str("[api.example.com, '*.cdn.net:80']").unwrap();
        assert_eq!(list.allow.len(), 2);
        let full: EgressSpec = serde_yaml_ng::from_str(
            "allow: [api.example.com]\nsecrets:\n  - GH@api.github.com\n  - {env: K, secret: store-k, hosts: [x.example.com:8443]}\n",
        )
        .unwrap();
        assert_eq!(full.secrets.len(), 2);
        assert_eq!(full.secrets[0].env, "GH");
        assert_eq!(full.secrets[1].secret.as_deref(), Some("store-k"));
        assert!(serde_yaml_ng::from_str::<EgressSpec>("open").is_err());
        let empty: EgressSpec = serde_yaml_ng::from_str("[]").unwrap();
        assert!(Policy::from_spec(&empty, "n").unwrap().is_none());
    }

    #[test]
    fn spec_roundtrips() {
        for text in ["none", "[a.example.com]"] {
            let s: EgressSpec = serde_yaml_ng::from_str(text).unwrap();
            let back: EgressSpec =
                serde_yaml_ng::from_str(&serde_yaml_ng::to_string(&s).unwrap()).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn policy_collects_secret_hosts_into_the_allow_list() {
        let spec = EgressSpec {
            allow: vec!["registry.npmjs.org".into()],
            none: false,
            secrets: vec![EgressSecretSpec::parse("GH_TOKEN=gh-prod@api.github.com").unwrap()],
        };
        let p = Policy::from_spec(&spec, "isbbrxabc").unwrap();
        assert!(p.allows("registry.npmjs.org", 443));
        assert!(p.allows("api.github.com", 443));
        assert!(p.intercepts("api.github.com", 443));
        assert!(!p.intercepts("registry.npmjs.org", 443));
        assert_eq!(p.secrets[0].secret, "gh-prod");
        assert_eq!(p.secrets[0].env, "GH_TOKEN");
        assert!(p.secrets[0].placeholder.starts_with("isb_placeholder_"));
        assert_eq!(p.ports().into_iter().collect::<Vec<_>>(), vec![443]);
    }

    #[test]
    fn secrets_are_validated() {
        let bad = |s: &str| {
            Policy::from_spec(
                &EgressSpec {
                    secrets: vec![EgressSecretSpec::parse(s).unwrap()],
                    ..Default::default()
                },
                "n",
            )
        };
        assert!(bad("1BAD@a.example.com").is_err());
        assert!(bad("OK@1.2.3.4").is_err());
        assert!(bad("OK=../x@a.example.com").is_err());
        assert!(EgressSecretSpec::parse("NOHOSTS").is_err());
        let both = EgressSpec {
            none: true,
            allow: vec!["a.example.com".into()],
            ..Default::default()
        };
        assert!(Policy::from_spec(&both, "n").is_err());
    }

    #[test]
    fn placeholders_are_stable_and_per_sandbox() {
        assert_eq!(placeholder("n1", "K"), placeholder("n1", "K"));
        assert_ne!(placeholder("n1", "K"), placeholder("n2", "K"));
        assert_ne!(placeholder("n1", "K"), placeholder("n1", "J"));
    }

    #[test]
    fn dns_names_cover_entries() {
        let p = Policy::from_spec(
            &EgressSpec::allow(["api.example.com", "*.cdn.net:80", "api.example.com:8443"]),
            "n",
        )
        .unwrap();
        assert_eq!(
            p.dns_names().into_iter().collect::<Vec<_>>(),
            vec!["api.example.com".to_string(), "cdn.net".to_string()]
        );
        assert_eq!(
            p.ports().into_iter().collect::<Vec<_>>(),
            vec![80, 443, 8443]
        );
    }

    #[test]
    fn pinned_host_needs_exactly_one_exact_entry_on_the_port() {
        let p = Policy::from_spec(
            &EgressSpec::allow([
                "db.example.com:5432",
                "a.example.com",
                "b.example.com",
                "*.w.net:9000",
            ]),
            "n",
        )
        .unwrap();
        assert_eq!(p.pinned_host(5432), Some("db.example.com"));
        assert_eq!(p.pinned_host(443), None, "two hosts share 443");
        assert_eq!(p.pinned_host(9000), None, "a suffix names no host");
        assert_eq!(p.pinned_host(1), None);
    }
}
