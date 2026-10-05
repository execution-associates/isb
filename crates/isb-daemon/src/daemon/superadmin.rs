//! Who is a superadmin ([`crate::auth::superadmin`]) on this daemon: a
//! superadmin token, a tailnet identity on `--superadmin-tailnet`, a
//! verified Cloudflare Access identity on `--superadmin-access`, either kind
//! of identity added to `isb.db` with `isb superadmin add` (read per request,
//! so no restart), or, in a debug build, any credential-less loopback request
//! under `ISB_DEV_SUPERADMIN` ([`crate::auth::dev`]). Nothing else grants it.
//! One gate serves the tool endpoints (through the authn hook) and the
//! identity endpoints (`/api/v1/auth/*`).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::auth::agent_identities::{AgentKind, AgentWays};
use crate::auth::edge::EdgeIdentity;
use crate::auth::superadmin::SuperadminIdentity;
use crate::auth::{AuthStore, Principal, Superadmin, SuperadminSource};
use crate::error::{Error, Result};
use crate::server::access::{ASSERTION_HEADER, AccessValidator, Identity};
use crate::server::http::{Peer, Request};
use crate::server::tailnet::{Tailnet, host_only};
use crate::server::{Registry, Tool};

/// `--superadmin-access`: Access emails and service-token client ids.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessAllowList {
    pub emails: Vec<String>,
    pub client_ids: Vec<String>,
}

impl AccessAllowList {
    /// Comma-separated; an entry with `@` is an email, else a service
    /// token's client id. Exact matches only: no wildcards or domains.
    pub fn parse(list: &str) -> Result<AccessAllowList> {
        let mut a = AccessAllowList::default();
        for e in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if e.contains(['*', '?', ' ', '\t']) || e.starts_with('@') || e.ends_with('@') {
                return Err(Error::invalid(format!(
                    "--superadmin-access: {e:?}: exact emails or service token client ids only"
                )));
            }
            if e.contains('@') {
                a.emails.push(e.to_ascii_lowercase());
            } else {
                a.client_ids.push(e.to_string());
            }
        }
        if a.emails.is_empty() && a.client_ids.is_empty() {
            return Err(Error::invalid(
                "--superadmin-access needs at least one email or service token client id",
            ));
        }
        Ok(a)
    }

    /// A user by email (case-insensitively); a service token by client id.
    pub fn admits(&self, id: &Identity) -> bool {
        match (&id.email, &id.common_name) {
            (Some(e), _) => self.emails.iter().any(|x| x.eq_ignore_ascii_case(e)),
            (None, Some(cn)) => self.client_ids.iter().any(|x| x == cn),
            (None, None) => false,
        }
    }

    pub fn entries(&self) -> Vec<String> {
        self.emails
            .iter()
            .chain(&self.client_ids)
            .cloned()
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.emails.is_empty() && self.client_ids.is_empty()
    }
}

/// One superadmin identity, as `superadmin_list` and the start-up line
/// show it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Listed {
    pub kind: AgentKind,
    pub value: String,
    /// `flag` (`--superadmin-access` / `--superadmin-tailnet`, read at
    /// start-up) or `state` (`isb.db`, `isb superadmin add`, read per request).
    pub source: &'static str,
    /// Whether a request can match it on this daemon at all.
    pub effective: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
}

const NO_ACCESS: &str = "not effective: an Access superadmin needs Cloudflare Access (CF_ACCESS_TEAM_DOMAIN and CF_ACCESS_AUD) and --public-url";
const NO_TAILNET: &str =
    "not effective: no tailnet --listen address, so no tailnet peer reaches this daemon";

/// What the gate makes of a request.
pub enum Resolved {
    /// Not a superadmin credential: judge the request as before.
    None,
    /// A superadmin token that is not valid.
    Refused,
    Superadmin(Arc<Superadmin>),
}

pub struct Gate {
    store: Arc<AuthStore>,
    tailnet: Option<Tailnet>,
    /// The tailnet `--listen` addresses (what lets a tailnet peer in at all).
    tailnet_listens: Vec<String>,
    /// The validator of the listeners Access guards, and the `Host` names an
    /// Access agent's request may carry (empty: no public URL, not checked).
    access_agents: Option<(Arc<AccessValidator>, Vec<String>)>,
    /// The Access validator of the loopback listeners, the
    /// `--superadmin-access` list (empty without the flag; `isb.db`'s
    /// entries count too), and the `Host` names an Access superadmin's
    /// request may carry. None: no Access superadmin is possible here.
    access: Option<(Arc<AccessValidator>, AccessAllowList, Vec<String>)>,
    /// `ISB_DEV_SUPERADMIN`: the email a loopback request with no
    /// credential is signed in as.
    #[cfg(debug_assertions)]
    dev: Option<String>,
}

impl Gate {
    pub fn new(
        store: Arc<AuthStore>,
        tailnet: Option<Tailnet>,
        access: Option<(Arc<AccessValidator>, AccessAllowList, Vec<String>)>,
    ) -> Gate {
        let access = access.map(|(v, a, hosts)| {
            let mut hosts: Vec<String> = hosts.iter().map(|h| host_only(h)).collect();
            hosts.sort();
            hosts.dedup();
            (v, a, hosts)
        });
        Gate {
            store,
            tailnet,
            tailnet_listens: Vec::new(),
            access_agents: None,
            access,
            #[cfg(debug_assertions)]
            dev: None,
        }
    }

    /// Sign every loopback request with no credential in as a superadmin
    /// acting as `email` ([`crate::auth::dev`]).
    #[cfg(debug_assertions)]
    pub fn with_dev(mut self, email: Option<String>) -> Gate {
        self.dev = email;
        self
    }

    /// Let orgs' tailnet and Access agent identities in
    /// ([`crate::auth::agent_identities`]): the tailnet `--listen`
    /// addresses, and Access's validator with the `Host` names to allow.
    pub fn with_agents(
        mut self,
        tailnet_listens: Vec<String>,
        access: Option<(Arc<AccessValidator>, Vec<String>)>,
    ) -> Gate {
        self.tailnet_listens = tailnet_listens;
        self.access_agents = access.map(|(v, hosts)| {
            let mut hosts: Vec<String> = hosts.iter().map(|h| host_only(h)).collect();
            hosts.sort();
            hosts.dedup();
            (v, hosts)
        });
        self
    }

    /// Which agent identities can reach this server at all.
    pub fn agent_ways(&self) -> AgentWays {
        AgentWays {
            tailnet_listen: if self.tailnet.is_some() {
                self.tailnet_listens.clone()
            } else {
                Vec::new()
            },
            access: self.access_agents.is_some(),
            public_url: None,
            superadmin_access: self
                .access_list()
                .map(AccessAllowList::entries)
                .unwrap_or_default(),
            superadmin_tailnet: self
                .tailnet
                .as_ref()
                .map(|t| t.allow().entries())
                .unwrap_or_default(),
        }
    }

    /// The agent identity behind `req`, if an org maps it: a verified
    /// Access identity (`id`, else the request's own assertion) on a
    /// loopback listener Access guards, or a tailnet peer, as tailscaled
    /// says. A bearer token decides on its own, so it is not asked here.
    /// Superadmin sources are judged first, by [`Gate::resolve`].
    pub fn agent(&self, req: &Request, id: Option<&Identity>) -> Option<Principal> {
        if req.header("authorization").is_some() {
            return None;
        }
        let looked = if matches!(&req.peer, Peer::Tcp(a) if a.ip().is_loopback()) {
            let (v, hosts) = self.access_agents.as_ref()?;
            let verified;
            let id = match id {
                Some(id) => id,
                None => {
                    let t = req.header(ASSERTION_HEADER)?.trim();
                    verified = v.validate(t).ok()?;
                    &verified
                }
            };
            if !hosts.is_empty() {
                let host = req.header("host").map(host_only).unwrap_or_default();
                if !hosts.contains(&host) {
                    eprintln!(
                        "isb serve: Access agent {} sent Host {host:?}, not one of this server's names; not an agent",
                        id.name()
                    );
                    return None;
                }
            }
            self.store
                .principal_for_access_agent(id.email.as_deref(), id.common_name.as_deref())
        } else {
            let w = self.tailnet.as_ref()?.identify(req)?;
            self.store.principal_for_tailnet(&w.login, &w.node, &w.tags)
        };
        looked.unwrap_or_else(|e| {
            eprintln!("isb serve: agent identities: {e}");
            None
        })
    }

    /// The person a front door verified ([`crate::auth::edge`]): a verified
    /// Access user on a loopback listener Access guards, or an untagged
    /// tailnet peer. Service tokens and tagged nodes are not people. They
    /// may claim setup unless this front door's superadmin allow list
    /// exists and leaves them out.
    pub fn edge(&self, req: &Request, id: Option<&Identity>) -> Option<EdgeIdentity> {
        if req.header("authorization").is_some() {
            return None;
        }
        if matches!(&req.peer, Peer::Tcp(a) if a.ip().is_loopback()) {
            let (v, hosts) = self.access_agents.as_ref()?;
            let verified;
            let id = match id {
                Some(id) => id,
                None => {
                    let t = req.header(ASSERTION_HEADER)?.trim();
                    verified = v.validate(t).ok()?;
                    &verified
                }
            };
            let email = id.email.as_deref()?.to_ascii_lowercase();
            if !hosts.is_empty() {
                let host = req.header("host").map(host_only).unwrap_or_default();
                if !hosts.contains(&host) {
                    return None;
                }
            }
            let can_claim = self.access.as_ref().is_none_or(|(_, flag, _)| {
                let state = self.state(AgentKind::Access);
                (flag.is_empty() && state.is_empty())
                    || flag.admits(id)
                    || state
                        .iter()
                        .any(|s| s.admits_access(id.email.as_deref(), id.common_name.as_deref()))
            });
            return Some(EdgeIdentity {
                kind: AgentKind::Access,
                subject: id.sub.clone(),
                name: email.clone(),
                email: Some(email),
                node: None,
                can_claim,
            });
        }
        let t = self.tailnet.as_ref()?;
        let w = t.identify(req)?;
        if !w.tags.is_empty() {
            return None;
        }
        let state = self.state(AgentKind::Tailnet);
        let listed = !t.allow().entries().is_empty() || !state.is_empty();
        let can_claim = !listed || self.tailnet_admits(&w, &state);
        Some(EdgeIdentity {
            kind: AgentKind::Tailnet,
            subject: w.login.clone(),
            name: w.login.clone(),
            email: crate::auth::edge::login_email(&w.login),
            node: Some(w.node.clone()),
            can_claim,
        })
    }

    /// [`Gate::agent`] and [`Gate::edge`] as the identity endpoints ask them.
    pub fn agent_fn(self: &Arc<Self>) -> crate::auth::http::AgentFn {
        let g = self.clone();
        Arc::new(move |r: &Request| g.agent(r, None))
    }

    pub fn edge_fn(self: &Arc<Self>) -> crate::auth::edge::EdgeFn {
        let g = self.clone();
        Arc::new(move |r: &Request| g.edge(r, None))
    }

    pub fn tailnet(&self) -> Option<&Tailnet> {
        self.tailnet.as_ref()
    }

    /// `--superadmin-access`, when given.
    pub fn access_list(&self) -> Option<&AccessAllowList> {
        self.access
            .as_ref()
            .map(|(_, a, _)| a)
            .filter(|a| !a.is_empty())
    }

    /// Every superadmin identity, the flags' then `isb.db`'s, each with
    /// whether this daemon can match it.
    pub fn listing(&self) -> Result<Vec<Listed>> {
        let access_on = self.access.is_some();
        let tailnet_on = self.tailnet.is_some() && !self.tailnet_listens.is_empty();
        let flag = |kind, value: String, on: bool, note| Listed {
            kind,
            value,
            source: "flag",
            effective: on,
            note: (!on).then_some(note),
            id: None,
            added_at: None,
            added_by: None,
        };
        let mut v: Vec<Listed> = Vec::new();
        for e in self
            .access_list()
            .map(AccessAllowList::entries)
            .unwrap_or_default()
        {
            v.push(flag(AgentKind::Access, e, true, NO_ACCESS));
        }
        for e in self
            .tailnet
            .as_ref()
            .map(|t| t.allow().entries())
            .unwrap_or_default()
        {
            v.push(flag(AgentKind::Tailnet, e, tailnet_on, NO_TAILNET));
        }
        for i in self.store.list_superadmin_identities()? {
            let (on, note) = match i.kind {
                AgentKind::Access => (access_on, NO_ACCESS),
                AgentKind::Tailnet => (tailnet_on, NO_TAILNET),
            };
            v.push(Listed {
                kind: i.kind,
                value: i.value,
                source: "state",
                effective: on,
                note: (!on).then_some(note),
                id: Some(i.id),
                added_at: Some(i.added_at),
                added_by: Some(i.added_by),
            });
        }
        Ok(v)
    }

    /// `isb.db`'s superadmin identities of `kind`, read now: the host CLI
    /// writes the table from another process, so a cached copy would need a
    /// restart. A failed read admits nobody, and says why.
    fn state(&self, kind: AgentKind) -> Vec<SuperadminIdentity> {
        match self.store.list_superadmin_identities() {
            Ok(v) => v.into_iter().filter(|i| i.kind == kind).collect(),
            Err(e) => {
                eprintln!("isb serve: superadmin identities in isb.db: {e}");
                Vec::new()
            }
        }
    }

    /// On `--superadmin-tailnet` or among `state`.
    fn tailnet_admits(
        &self,
        w: &crate::server::tailnet::Whois,
        state: &[SuperadminIdentity],
    ) -> bool {
        self.tailnet.as_ref().is_some_and(|t| t.allow().admits(w))
            || state.iter().any(|s| s.admits_tailnet(&w.login, &w.tags))
    }

    /// `id` is the Access identity the listener already verified, if any.
    /// A bearer superadmin token decides alone; any other bearer token is
    /// not this gate's. Then Access, then the tailnet, then (debug builds)
    /// `ISB_DEV_SUPERADMIN`.
    pub fn resolve(&self, req: &Request, id: Option<&Identity>) -> Resolved {
        if let Some(a) = req.header("authorization") {
            let token = a
                .trim()
                .split_once(' ')
                .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
                .map(|(_, t)| t.trim());
            return match token {
                Some(t) if t.starts_with(crate::auth::secret::TokenKind::Superadmin.prefix()) => {
                    match self.store.authenticate_superadmin_token(t) {
                        Ok(Some(info)) => Resolved::Superadmin(Arc::new(Superadmin::synthetic(
                            SuperadminSource::Token {
                                id: info.id,
                                name: info.name,
                            },
                        ))),
                        Ok(None) => Resolved::Refused,
                        Err(e) => {
                            eprintln!("isb serve: superadmin token: {e}");
                            Resolved::Refused
                        }
                    }
                }
                _ => Resolved::None,
            };
        }
        if let Some(s) = self.access_superadmin(req, id) {
            return Resolved::Superadmin(s);
        }
        let tailnet = self.tailnet.as_ref().and_then(|t| t.identify(req));
        if let Some(w) = tailnet.filter(|w| self.tailnet_admits(w, &self.state(AgentKind::Tailnet)))
        {
            let source = SuperadminSource::Tailnet {
                login: w.login.clone(),
                node: w.node,
                tags: w.tags.clone(),
            };
            let as_user = if w.tags.is_empty() {
                Some(w.login.as_str())
            } else {
                None
            };
            return Resolved::Superadmin(Arc::new(self.acting_as(source, as_user)));
        }
        #[cfg(debug_assertions)]
        if let Some(s) = self.dev_superadmin(req, id) {
            return Resolved::Superadmin(s);
        }
        Resolved::None
    }

    /// A loopback TCP request with no credential of any kind: no bearer
    /// token (judged above), session cookie or Access assertion. The unix
    /// socket is its own caller.
    #[cfg(debug_assertions)]
    fn dev_superadmin(&self, req: &Request, id: Option<&Identity>) -> Option<Arc<Superadmin>> {
        let email = self.dev.as_deref()?;
        let loopback = matches!(&req.peer, Peer::Tcp(a) if a.ip().is_loopback());
        let credential = id.is_some()
            || req.header(ASSERTION_HEADER).is_some()
            || crate::auth::http::cookie(req, crate::auth::http::COOKIE).is_some();
        if !loopback || credential {
            return None;
        }
        let source = SuperadminSource::Dev {
            email: email.to_string(),
        };
        Some(Arc::new(self.acting_as(source, Some(email))))
    }

    /// Only from a verified assertion, on the loopback listeners Access
    /// guards.
    fn access_superadmin(&self, req: &Request, id: Option<&Identity>) -> Option<Arc<Superadmin>> {
        let (v, allow, hosts) = self.access.as_ref()?;
        let loopback = matches!(&req.peer, Peer::Tcp(a) if a.ip().is_loopback());
        if !loopback {
            return None;
        }
        let state = self.state(AgentKind::Access);
        if allow.is_empty() && state.is_empty() {
            return None;
        }
        let verified;
        let id = match id {
            Some(id) => id,
            None => {
                let t = req.header(ASSERTION_HEADER)?.trim();
                verified = v.validate(t).ok()?;
                &verified
            }
        };
        let listed = allow.admits(id)
            || state
                .iter()
                .any(|s| s.admits_access(id.email.as_deref(), id.common_name.as_deref()));
        if !listed {
            return None;
        }
        let host = req.header("host").map(host_only).unwrap_or_default();
        if !hosts.contains(&host) {
            eprintln!(
                "isb serve: Access superadmin {} sent Host {host:?}, not one of this server's names; not a superadmin",
                id.name()
            );
            return None;
        }
        let source = SuperadminSource::Access {
            name: id.name().to_string(),
            service_token: id.is_service_token(),
        };
        Some(Arc::new(self.acting_as(source, id.email.as_deref())))
    }

    /// As the enabled isb user with this email, else synthetic.
    fn acting_as(&self, source: SuperadminSource, email: Option<&str>) -> Superadmin {
        let user = email
            .and_then(|e| self.store.user_by_email(e).ok().flatten())
            .filter(|u| !u.disabled);
        match user {
            Some(u) => Superadmin::as_user(source.clone(), u, &self.store)
                .unwrap_or_else(|_| Superadmin::synthetic(source)),
            None => Superadmin::synthetic(source),
        }
    }
}

/// Tools for superadmins only (not platform admins): the host itself, and
/// what reaches further into its kernel (nesting for an org's workspace).
pub const TOOLS: &[&str] = &[
    "host_inventory",
    "host_policy",
    "superadmin_token_list",
    "superadmin_token_revoke",
    "superadmin_list",
    "org_nesting",
];

/// A `--listen` address on the tailnet (it then binds without a tunnel).
pub fn is_tailnet_listen(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    addr.to_socket_addrs().is_ok_and(|mut a| {
        a.next()
            .is_some_and(|a| crate::server::tailnet::is_tailnet_ip(a.ip()))
    })
}

/// The public URL's host, for the `Host` checks.
fn public_host(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// The gate `cfg` describes. Refuses `--superadmin-access` without Access or a
/// public URL (whose host the Access check needs).
pub fn gate(
    cfg: &super::ServeConfig,
    store: Arc<AuthStore>,
    access: Option<Arc<AccessValidator>>,
) -> Result<Gate> {
    let tailnet_listens: Vec<&String> =
        cfg.listen.iter().filter(|a| is_tailnet_listen(a)).collect();
    let public = cfg.public_url.as_deref().and_then(public_host);
    if cfg.superadmin_tailnet.is_some() && tailnet_listens.is_empty() {
        eprintln!(
            "isb serve: WARNING: --superadmin-tailnet without a tailnet --listen address: no tailnet peer can reach this daemon"
        );
    }
    // The tailnet check runs wherever a tailnet address is served: for the
    // superadmin allow list, and for orgs' agent identities.
    let tailnet = (cfg.superadmin_tailnet.is_some() || !tailnet_listens.is_empty()).then(|| {
        let allow = cfg.superadmin_tailnet.clone().unwrap_or_default();
        let mut hosts: Vec<String> = tailnet_listens.iter().map(|a| a.to_string()).collect();
        hosts.extend(crate::server::tailnet::self_names());
        hosts.extend(public.clone());
        crate::server::tailnet::Tailnet::new(allow, crate::server::tailnet::system_fetcher(), hosts)
    });
    let mut access_hosts: Vec<String> = public.iter().cloned().collect();
    access_hosts.extend(cfg.listen.iter().filter(|a| !is_tailnet_listen(a)).cloned());
    let agent_access = access.clone().map(|v| {
        let hosts = if public.is_some() {
            access_hosts
        } else {
            Vec::new()
        };
        (v, hosts)
    });
    // Access superadmins are possible wherever Access and the public host
    // are: the flag's list, else only `isb superadmin add`'s.
    let access = match (&cfg.superadmin_access, access) {
        (None, Some(v)) => public.clone().map(|host| {
            let mut hosts = vec![host];
            hosts.extend(cfg.listen.iter().filter(|a| !is_tailnet_listen(a)).cloned());
            (v, AccessAllowList::default(), hosts)
        }),
        (None, None) => None,
        (Some(_), None) => {
            return Err(Error::invalid(
                "--superadmin-access needs Cloudflare Access (CF_ACCESS_TEAM_DOMAIN and CF_ACCESS_AUD): it trusts only a verified assertion",
            ));
        }
        (Some(list), Some(v)) => {
            let Some(host) = public.clone() else {
                return Err(Error::invalid(
                    "--superadmin-access needs --public-url: an Access superadmin's request must name this server's host",
                ));
            };
            let mut hosts = vec![host];
            hosts.extend(cfg.listen.iter().filter(|a| !is_tailnet_listen(a)).cloned());
            Some((v, list.clone(), hosts))
        }
    };
    let listens = tailnet_listens.iter().map(|a| a.to_string()).collect();
    let gate = Gate::new(store, tailnet, access).with_agents(listens, agent_access);
    #[cfg(debug_assertions)]
    let gate = gate.with_dev(dev_superadmin(cfg)?);
    #[cfg(not(debug_assertions))]
    if cfg.dev_superadmin.is_some() {
        return Err(Error::invalid(format!(
            "{} works only in debug builds: unset it",
            crate::auth::dev::SUPERADMIN_ENV
        )));
    }
    Ok(gate)
}

/// `ISB_DEV_SUPERADMIN`, refused unless every `--listen` address is
/// loopback: it signs in anyone who can connect.
#[cfg(debug_assertions)]
fn dev_superadmin(cfg: &super::ServeConfig) -> Result<Option<String>> {
    use std::net::ToSocketAddrs;
    let Some(email) = cfg.dev_superadmin.clone() else {
        return Ok(None);
    };
    let env = crate::auth::dev::SUPERADMIN_ENV;
    let loopback = |a: &String| {
        a.to_socket_addrs()
            .is_ok_and(|mut it| it.all(|s| s.ip().is_loopback()))
    };
    if cfg.listen.is_empty() || !cfg.listen.iter().all(loopback) {
        return Err(Error::invalid(format!(
            "{env} signs every request without a credential in as a superadmin: --listen must be loopback only (it is {:?})",
            cfg.listen
        )));
    }
    eprintln!(
        "isb serve: WARNING: {env}={email}: every HTTP request without a credential is superadmin dev:{email}; for developing isb only"
    );
    Ok(Some(email))
}

/// What `host_policy` reports of the configuration (never a secret).
pub fn host_summary(cfg: &super::ServeConfig, gate: &Gate) -> Value {
    json!({
        "isb": env!("CARGO_PKG_VERSION"),
        "listen": cfg.listen,
        "socket": cfg.socket,
        "state_dir": cfg.state_dir,
        "public_url": cfg.public_url,
        "access": cfg.access.as_ref().map(|(team, aud)| json!({"team_domain": team, "aud": aud})),
        "allow_unauthenticated": cfg.allow_unauthenticated,
        "tools": {"allow": cfg.remote_tools.allow, "deny": cfg.remote_tools.deny},
        "policy": {
            "allow_privileged": cfg.policy.allow_privileged,
            "allow_raw": cfg.policy.allow_raw,
            "bind_roots": cfg.policy.bind_roots,
            "publish_addresses": cfg.policy.publish_addresses,
            "any_instance": cfg.policy.any_instance,
        },
        "superadmin": {
            "socket": cfg.socket,
            "tokens": true,
            "tailnet": cfg.superadmin_tailnet.as_ref().and(gate.tailnet()).map(|t| t.allow().entries()),
            "tailnet_hosts": gate.tailnet().map(|t| t.hosts().to_vec()),
            "access": gate.access_list().map(AccessAllowList::entries),
        },
    })
}

/// Say at start-up which sources grant superadmin.
pub fn announce(cfg: &super::ServeConfig, gate: &Gate, store: &AuthStore) {
    let mut v = vec![format!("the unix socket {}", cfg.socket.display())];
    if !cfg.listen.is_empty() {
        let n = store.list_superadmin_tokens().map(|t| t.len()).unwrap_or(0);
        v.push(format!(
            "superadmin tokens ({n}; minted on this host with `isb token create NAME --superadmin`)"
        ));
    }
    if let Some(t) = cfg.superadmin_tailnet.as_ref().and(gate.tailnet()) {
        v.push(format!(
            "tailnet identities {} (--superadmin-tailnet; Host: {})",
            t.allow().entries().join(", "),
            t.hosts().join(", ")
        ));
    }
    if let Some(a) = gate.access_list() {
        v.push(format!(
            "Cloudflare Access identities {} (--superadmin-access)",
            a.entries().join(", ")
        ));
    }
    let state: Vec<Listed> = match gate.listing() {
        Ok(l) => l.into_iter().filter(|l| l.source == "state").collect(),
        Err(e) => {
            eprintln!("isb serve: superadmin identities in isb.db: {e}");
            Vec::new()
        }
    };
    for (kind, what) in [
        (AgentKind::Tailnet, "tailnet identities"),
        (AgentKind::Access, "Cloudflare Access identities"),
    ] {
        let on: Vec<&str> = state
            .iter()
            .filter(|l| l.kind == kind && l.effective)
            .map(|l| l.value.as_str())
            .collect();
        if !on.is_empty() {
            v.push(format!(
                "{what} {} (isb.db: isb superadmin add)",
                on.join(", ")
            ));
        }
    }
    #[cfg(debug_assertions)]
    if let Some(e) = &gate.dev {
        v.push(format!(
            "every loopback request without a credential, as {e} ({})",
            crate::auth::dev::SUPERADMIN_ENV
        ));
    }
    eprintln!("isb serve: superadmins: {}", v.join("; "));
    for l in state.iter().filter(|l| !l.effective) {
        eprintln!(
            "isb serve: WARNING: superadmin {} {} (isb.db) is {}",
            l.kind.as_str(),
            l.value,
            l.note.unwrap_or("not effective")
        );
    }
}

/// The superadmin-only tools.
pub(super) fn register(r: &mut Registry, d: Arc<super::Daemon>) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let empty = || json!({"type": "object", "properties": {"org": {"type": "string"}}, "additionalProperties": false});
    let dd = d.clone();
    r.register(
        Tool::new(
            "host_inventory",
            "Every incus project and instance on the host, isb's or not: projects with the org each one is (if any); instances with project, type, status, addresses, and isb's labels (stack, owner). Superadmins only.",
            empty(),
            move |_a, _c| inventory(&dd),
        )
        .title("Host inventory")
        .annotations(ro.clone()),
    )?;
    let dd = d.clone();
    r.register(
        Tool::new(
            "host_policy",
            "How this daemon serves: listen addresses, Cloudflare Access, the tools remote callers see, what a remote caller's specs may ask for (bind roots, publish addresses, privileged, raw, any instance), and every superadmin source with its allow lists. Superadmins only.",
            empty(),
            move |_a, _c| {
                let mut v = dd.host.clone();
                v["superadmin"]["token_count"] =
                    json!(dd.users.list_superadmin_tokens().map(|t| t.len()).unwrap_or(0));
                v["superadmin"]["identities"] = json!(dd.gate.listing()?);
                Ok(v)
            },
        )
        .title("Host policy")
        .annotations(ro.clone()),
    )?;
    let dd = d.clone();
    r.register(
        Tool::new(
            "superadmin_token_list",
            "Superadmin tokens: id, name, created, last used, expiry (never the token). They are minted only on the host: isb token create NAME --superadmin. Superadmins only.",
            empty(),
            move |_a, _c| Ok(json!({"tokens": dd.users.list_superadmin_tokens()?})),
        )
        .title("Superadmin tokens")
        .annotations(ro.clone()),
    )?;
    let dd = d.clone();
    r.register(
        Tool::new(
            "superadmin_list",
            "Superadmin identities: tailnet logins and tags, Cloudflare Access emails and service token client ids, each with its source (`flag`: --superadmin-tailnet / --superadmin-access, read at start-up; `state`: isb.db, added with `isb superadmin add` and read per request) and whether this daemon can match it (`effective`, with a `note` when not). Read only: identities are added and removed only on the host (isb superadmin add / rm), never over HTTP. Superadmins only.",
            empty(),
            move |_a, _c| Ok(json!({"identities": dd.gate.listing()?})),
        )
        .title("Superadmin identities")
        .annotations(ro),
    )?;
    let dd = d;
    r.register(
        Tool::new(
            "superadmin_token_revoke",
            "Revoke a superadmin token by id; it stops working at once. Superadmins only.",
            json!({"type": "object", "properties": {"id": {"type": "integer"}, "org": {"type": "string"}}, "required": ["id"], "additionalProperties": false}),
            move |a, _c| {
                let id = a
                    .get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| Error::invalid("id: an integer"))?;
                let t = dd.users.superadmin_token(id)?;
                dd.users.revoke_superadmin_token(id)?;
                Ok(json!({"revoked": t}))
            },
        )
        .title("Revoke a superadmin token")
        .annotations(destructive),
    )?;
    Ok(())
}

fn inventory(d: &super::Daemon) -> Result<Value> {
    let projects = d.client.get("/1.0/projects?recursion=1")?;
    let projects: Vec<Value> = projects
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    let name = p["name"].as_str().unwrap_or("");
                    json!({
                        "name": name,
                        "description": p["description"],
                        "org": crate::org::OrgId::from_incus_project(name).map(|o| o.to_string()),
                        "instances": p["used_by"].as_array().map(|u| u.iter().filter(|x| x.as_str().is_some_and(|s| s.starts_with("/1.0/instances/"))).count()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let instances = d
        .client
        .get("/1.0/instances?recursion=2&all-projects=true")?;
    let instances: Vec<Value> = instances
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| {
                    let project = i["project"].as_str().unwrap_or("default");
                    let cfg = &i["config"];
                    let label = |k: &str| cfg.get(format!("user.{k}")).cloned().unwrap_or(Value::Null);
                    let addresses: Vec<String> = i["state"]["network"]
                        .as_object()
                        .map(|n| {
                            n.iter()
                                .filter(|(k, _)| k.as_str() != "lo")
                                .flat_map(|(_, v)| v["addresses"].as_array().cloned().unwrap_or_default())
                                .filter(|a| a["scope"] == "global")
                                .filter_map(|a| a["address"].as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let stack = label("isb.stack");
                    let owner = label(super::LABEL_OWNER);
                    json!({
                        "name": i["name"],
                        "project": project,
                        "org": crate::org::OrgId::from_incus_project(project).map(|o| o.to_string()),
                        "type": i["type"],
                        "status": i["status"],
                        "created_at": i["created_at"],
                        "image": cfg.get("image.description").cloned().unwrap_or(Value::Null),
                        "addresses": addresses,
                        "stack": stack,
                        "owner": owner,
                        "managed": !stack.is_null() || !owner.is_null(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({"projects": projects, "instances": instances}))
}

#[cfg(test)]
mod tests;
