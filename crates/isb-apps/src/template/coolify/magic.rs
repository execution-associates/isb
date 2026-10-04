//! Coolify's "magic" variables and compose interpolation, as isb template
//! variables and expressions.
//!
//! `SERVICE_URL_<NAME>[_<PORT>]` and `SERVICE_FQDN_<NAME>[_<PORT>]` give a
//! service a domain (the service whose `environment:` declares the name; a
//! name nothing declares falls back to the service called that);
//! `SERVICE_PASSWORD_*`, `SERVICE_USER_*`, `SERVICE_BASE64_*`,
//! `SERVICE_REALBASE64_*`, `SERVICE_HEX_*` and the Supabase JWTs are values
//! generated once per deployment and shared by every use of the same name;
//! `SERVICE_NAME_<SERVICE>` is a service's name. Everything else in
//! `${...}` / `$NAME` is an ordinary variable with an optional default.

use std::collections::{BTreeMap, BTreeSet};

use serde_yaml_ng::Value as Y;

use super::super::shared::{Tx, pairs, secretish, var_name, yget, yscalar};
use super::super::{JwtSpec, VarKind, Variable};

/// How a generated value is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gen {
    /// Letters and digits; `symbols` when Coolify's has punctuation too.
    Password {
        len: u32,
        symbols: bool,
    },
    User,
    /// `BASE64`: despite the name, letters and digits.
    Alnum(u32),
    /// `REALBASE64`: base64 of this many random bytes.
    RealBase64(u32),
    /// `HEX`: this many hexadecimal characters.
    Hex(u32),
    /// A Supabase JWT with this role, signed with `SERVICE_PASSWORD_JWT`.
    Jwt(&'static str),
}

/// What a `SERVICE_*` name means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Magic {
    Url {
        name: String,
        port: Option<u16>,
    },
    Fqdn {
        name: String,
        port: Option<u16>,
    },
    /// A service's name, as `SERVICE_NAME_<SERVICE>` spells it.
    Name(String),
    Gen(Gen),
}

/// A trailing `_<port>` on a URL or FQDN name.
fn split_port(s: &str) -> (String, Option<u16>) {
    if let Some((name, port)) = s.rsplit_once('_') {
        if let Ok(p) = port.parse::<u16>() {
            if p > 0 && !name.is_empty() {
                return (name.to_string(), Some(p));
            }
        }
    }
    (s.to_string(), None)
}

/// `32_ID` / `64_ID` / `128_ID` as (size, ID); `ID` alone has the default
/// size.
fn sized<'a>(tail: &'a str, sizes: &[u32], default: u32) -> (u32, &'a str) {
    for n in sizes {
        if let Some(rest) = tail.strip_prefix(&format!("{n}_")) {
            if !rest.is_empty() {
                return (*n, rest);
            }
        }
    }
    (default, tail)
}

/// The meaning of a name such as `SERVICE_PASSWORD_64_UMAMI`, or `None`
/// for an ordinary variable.
pub fn classify(token: &str) -> Option<Magic> {
    let rest = token.strip_prefix("SERVICE_")?;
    if !rest
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let (kind, tail) = rest.split_once('_')?;
    if tail.is_empty() {
        return None;
    }
    Some(match kind {
        "URL" => {
            let (name, port) = split_port(tail);
            Magic::Url { name, port }
        }
        "FQDN" => {
            let (name, port) = split_port(tail);
            Magic::Fqdn { name, port }
        }
        "NAME" => Magic::Name(tail.to_string()),
        "USER" | "LOWERCASEUSER" => Magic::Gen(Gen::User),
        "PASSWORD" | "PASSWORDWITHSYMBOLS" => Magic::Gen(Gen::Password {
            len: sized(tail, &[64], 32).0,
            symbols: kind == "PASSWORDWITHSYMBOLS",
        }),
        "BASE64" => Magic::Gen(Gen::Alnum(sized(tail, &[32, 64, 128], 32).0)),
        "REALBASE64" => Magic::Gen(Gen::RealBase64(sized(tail, &[32, 64, 128], 32).0)),
        "HEX" => {
            let (n, id) = sized(tail, &[32, 64, 128], 0);
            if n == 0 || id.is_empty() {
                return None;
            }
            Magic::Gen(Gen::Hex(n))
        }
        "SUPABASEANON" => Magic::Gen(Gen::Jwt("anon")),
        "SUPABASESERVICE" => Magic::Gen(Gen::Jwt("service_role")),
        _ => return None,
    })
}

/// A service name as `SERVICE_NAME_*` and URL names spell it.
pub fn normalized(service: &str) -> String {
    service
        .to_ascii_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// One service's claim on a domain: its `environment:` names
/// `SERVICE_URL_<name>[_<port>]` (or FQDN).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    /// The compose service.
    pub service: String,
    pub name: String,
    pub port: Option<u16>,
    /// A path the value gives (`SERVICE_URL_API=/v1`).
    pub path: Option<String>,
}

/// Every `SERVICE_*` name in a YAML value, keys and values, and every
/// `${NAME` / `$NAME` outside `content:` blocks.
pub fn scan(v: &Y, tokens: &mut BTreeSet<String>, refs: &mut BTreeSet<String>) {
    match v {
        Y::String(s) => scan_text(s, tokens, refs),
        Y::Sequence(s) => s.iter().for_each(|x| scan(x, tokens, refs)),
        Y::Mapping(m) => {
            for (k, x) in m {
                if let Some(k) = k.as_str() {
                    scan_text(k, tokens, refs);
                    if k == "content" {
                        // Only magic names are read in file contents.
                        if let Some(s) = x.as_str() {
                            scan_text(s, tokens, &mut BTreeSet::new());
                        }
                        continue;
                    }
                }
                scan(x, tokens, refs);
            }
        }
        _ => {}
    }
}

fn name_at(s: &str, from: usize) -> &str {
    let rest = &s[from..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

fn scan_text(s: &str, tokens: &mut BTreeSet<String>, refs: &mut BTreeSet<String>) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // Magic names are found anywhere a word starts.
        let word_start = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if word_start && s[i..].starts_with("SERVICE_") {
            let t = name_at(s, i);
            if classify(t).is_some() {
                tokens.insert(t.to_string());
            }
            i += t.len().max(1);
            continue;
        }
        if b[i] == b'$' && b.get(i + 1) == Some(&b'$') {
            i += 2;
            continue;
        }
        if b[i] == b'$' {
            let from = if b.get(i + 1) == Some(&b'{') {
                i + 2
            } else {
                i + 1
            };
            let n = name_at(s, from);
            if n.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                refs.insert(n.to_string());
            }
        }
        i += 1;
        while !s.is_char_boundary(i) {
            i += 1;
        }
    }
}

/// The domains each service's `environment:` declares, in compose order.
pub fn declarations(
    services: &serde_yaml_ng::Mapping,
    skipped: &dyn Fn(&str) -> bool,
) -> Vec<Decl> {
    let mut out = Vec::new();
    for (name, s) in services {
        let Some(name) = yscalar(name) else { continue };
        if skipped(&name) {
            continue;
        }
        let Some(env) = s.as_mapping().and_then(|m| yget(m, "environment")) else {
            continue;
        };
        for (k, v) in pairs(env) {
            let (name_, port) = match classify(&k) {
                Some(Magic::Url { name, port } | Magic::Fqdn { name, port }) => (name, port),
                _ => continue,
            };
            let v = v.unwrap_or_default();
            // A value that is another expression is a pass-through, not a
            // claim.
            if !(v.is_empty() || v.starts_with('/')) {
                continue;
            }
            let path = (v.len() > 1).then_some(v);
            out.push(Decl {
                service: name.clone(),
                name: name_,
                port,
                path,
            });
        }
    }
    out
}

/// How a piece of text is read: compose interpolation everywhere, but file
/// contents expand only the names Coolify knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Compose,
    Content,
}

/// What interpolation needs to know about the template.
pub struct Cx<'a> {
    /// Compose service name to app key.
    pub keys: &'a BTreeMap<String, String>,
    /// Every name a service answers to, for rewriting host names.
    pub aliases: &'a [(String, String)],
    /// The template's `# port:`.
    pub port_hint: Option<u16>,
    /// Normalized service name to compose service name.
    by_norm: BTreeMap<String, String>,
    pub decls: Vec<Decl>,
    /// Domain name to the native variable holding its host.
    hosts: BTreeMap<String, String>,
    /// Variables the compose file interpolates outside file contents.
    known: BTreeSet<String>,
}

impl<'a> Cx<'a> {
    /// Read the declarations and references of the compose document, and
    /// make a domain variable for every domain name.
    pub fn new(
        doc: &Y,
        services: &serde_yaml_ng::Mapping,
        names: (&'a BTreeMap<String, String>, &'a [(String, String)]),
        port_hint: Option<u16>,
        tx: &mut Tx,
    ) -> Cx<'a> {
        let (keys, aliases) = names;
        let by_norm: BTreeMap<String, String> =
            keys.keys().map(|s| (normalized(s), s.clone())).collect();
        let mut decls = declarations(services, &|s| !keys.contains_key(s));
        let (mut tokens, mut known) = (BTreeSet::new(), BTreeSet::new());
        scan(doc, &mut tokens, &mut known);
        known.retain(|n| classify(n).is_none());
        for t in tokens {
            let (name, port) = match classify(&t) {
                Some(Magic::Url { name, port } | Magic::Fqdn { name, port }) => (name, port),
                _ => continue,
            };
            if decls.iter().any(|d| d.name == name) {
                continue;
            }
            // Nothing declares it: the service of that name gets it.
            let owner = by_norm.get(&name).map(|s| (s, port)).or_else(|| {
                by_norm
                    .get(&format!("{name}_{}", port.unwrap_or(0)))
                    .map(|s| (s, None))
            });
            if let Some((svc, port)) = owner {
                decls.push(Decl {
                    service: svc.clone(),
                    name,
                    port,
                    path: None,
                });
            }
        }
        let mut hosts = BTreeMap::new();
        for d in &decls {
            if !hosts.contains_key(&d.name) {
                let n = tx.fresh_name(&format!("domain_{}", d.name));
                tx.vars.push(Variable {
                    name: n.clone(),
                    kind: VarKind::Domain,
                    ..Default::default()
                });
                hosts.insert(d.name.clone(), n);
            }
        }
        Cx {
            keys,
            aliases,
            port_hint,
            by_norm,
            decls,
            hosts,
            known,
        }
    }

    /// The native variable holding a domain name's host.
    pub fn host_var(&self, name: &str) -> Option<&str> {
        self.hosts.get(name).map(String::as_str)
    }

    /// The path a declaration of this exact name gives.
    fn path_of(&self, name: &str, port: Option<u16>) -> Option<&str> {
        self.decls
            .iter()
            .find(|d| d.name == name && d.port == port && d.path.is_some())
            .and_then(|d| d.path.as_deref())
    }

    /// An ordinary variable's native name (made on first use).
    pub fn var(&self, name: &str, tx: &mut Tx) -> String {
        if let Some(n) = tx.names.get(name) {
            return n.clone();
        }
        let n = tx.fresh_name(name);
        tx.names.insert(name.to_string(), n.clone());
        tx.vars.push(Variable {
            name: n.clone(),
            ..Default::default()
        });
        n
    }

    /// A generated value's native variable (made on first use).
    fn generated(&self, token: &str, g: &Gen, tx: &mut Tx) -> String {
        if let Some(n) = tx.names.get(token) {
            return n.clone();
        }
        let secret_var = match g {
            Gen::Jwt(_) => Some(self.generated(
                "SERVICE_PASSWORD_JWT",
                &Gen::Password {
                    len: 32,
                    symbols: false,
                },
                tx,
            )),
            _ => None,
        };
        let n = tx.fresh_name(&var_name(token.trim_start_matches("SERVICE_")));
        let mut v = Variable {
            name: n.clone(),
            ..Default::default()
        };
        match g {
            Gen::Password { len, symbols } => {
                v.kind = VarKind::Password;
                v.length = Some(*len);
                if *symbols {
                    tx.note(format!("{token} has no symbols (letters and digits only)"));
                }
            }
            Gen::User => {
                v.kind = VarKind::Username;
                v.length = Some(16);
            }
            Gen::Alnum(len) => {
                v.kind = VarKind::Password;
                v.length = Some(*len);
            }
            Gen::RealBase64(bytes) => {
                v.kind = VarKind::Base64;
                v.bytes = Some(*bytes);
            }
            Gen::Hex(chars) => {
                v.kind = VarKind::Hex;
                v.bytes = Some(chars / 2);
                v.secret = Some(true);
            }
            Gen::Jwt(role) => {
                v.kind = VarKind::Jwt;
                v.jwt = Some(JwtSpec {
                    secret: secret_var.unwrap_or_default(),
                    payload: Some(format!("{{\"role\":\"{role}\",\"iss\":\"supabase\"}}")),
                });
            }
        }
        tx.names.insert(token.to_string(), n.clone());
        tx.vars.push(v);
        n
    }

    /// A magic name's value as a native expression.
    pub fn magic(&self, token: &str, m: &Magic, tx: &mut Tx) -> String {
        match m {
            Magic::Url { name, port } | Magic::Fqdn { name, port } => {
                let Some(h) = self.host_var(name) else {
                    tx.refuse(format!("{token}: no service declares or is named {name}"));
                    return String::new();
                };
                if matches!(m, Magic::Fqdn { .. }) {
                    format!("${{{h}}}")
                } else {
                    let path = self.path_of(name, *port).unwrap_or("");
                    format!("https://${{{h}}}{}", path.replace('$', "$$"))
                }
            }
            Magic::Name(n) => match self.by_norm.get(n).and_then(|s| self.keys.get(s)) {
                Some(k) => format!("${{host:{k}}}"),
                None => {
                    tx.refuse(format!("{token}: no service named {n}"));
                    String::new()
                }
            },
            Magic::Gen(g) => format!("${{{}}}", self.generated(token, g, tx)),
        }
    }

    /// A variable's default, the first one seen wins.
    fn set_default(&self, native: &str, d: String, tx: &mut Tx) {
        let Some(v) = tx.vars.iter_mut().find(|v| v.name == native) else {
            return;
        };
        match &v.default {
            None => v.default = Some(d),
            Some(e) if *e != d => {
                let msg = format!(
                    "{native} has different defaults in different places; the first is used"
                );
                tx.note(msg);
            }
            Some(_) => {}
        }
    }

    /// `${name<op>}` as a native expression.
    fn reference(&self, name: &str, op: &str, tx: &mut Tx) -> String {
        if let Some(m) = classify(name) {
            return self.magic(name, &m, tx);
        }
        let native = self.var(name, tx);
        if let Some(d) = op.strip_prefix(":-").or_else(|| op.strip_prefix('-')) {
            let d = self.expr(d, Mode::Compose, tx);
            self.set_default(&native, d, tx);
        } else if op.starts_with(":?") || op.starts_with('?') {
            if let Some(v) = tx.vars.iter_mut().find(|v| v.name == native) {
                v.required = Some(true);
            }
        } else if !op.is_empty() {
            tx.refuse(format!("${{{name}{op}}}: conditional interpolation"));
        }
        format!("${{{native}}}")
    }

    /// Whether `${name}` is expanded in file contents.
    fn expands_in_content(&self, name: &str) -> bool {
        classify(name).is_some() || self.known.contains(name)
    }

    /// Interpolate a compose value (or file content) into a native
    /// expression: references become variables, the rest is literal.
    pub fn expr(&self, s: &str, mode: Mode, tx: &mut Tx) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < s.len() {
            let Some(off) = s[i..].find('$') else {
                out.push_str(&s[i..]);
                break;
            };
            out.push_str(&s[i..i + off]);
            i += off;
            i = self.dollar(s, i, mode, &mut out, tx);
        }
        out
    }

    /// The `$...` at `s[i..]`, written to `out`; the index after it.
    fn dollar(&self, s: &str, i: usize, mode: Mode, out: &mut String, tx: &mut Tx) -> usize {
        let rest = &s[i + 1..];
        if rest.starts_with('$') {
            out.push_str(if mode == Mode::Compose { "$$" } else { "$$$$" });
            return i + 2;
        }
        if rest.starts_with('{') {
            let Some(end) = matching_brace(s, i + 1) else {
                if mode == Mode::Compose {
                    tx.refuse(format!("unterminated ${{ in {s:?}"));
                }
                out.push_str("$$");
                return i + 1;
            };
            let inner = &s[i + 2..end];
            let name = name_at(inner, 0);
            let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
            if valid && (mode == Mode::Compose || self.expands_in_content(name)) {
                out.push_str(&self.reference(name, &inner[name.len()..], tx));
            } else if mode == Mode::Compose {
                tx.refuse(format!("${{{inner}}} is not a variable"));
            } else {
                out.push_str(&s[i..=end].replace('$', "$$"));
            }
            return end + 1;
        }
        let name = name_at(s, i + 1);
        let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
        if valid && (mode == Mode::Compose || classify(name).is_some()) {
            out.push_str(&self.reference(name, "", tx));
            return i + 1 + name.len();
        }
        out.push_str("$$");
        i + 1
    }

    /// A bare environment entry's value: the magic value, or the variable
    /// of that name.
    pub fn bare(&self, name: &str, tx: &mut Tx) -> String {
        self.reference(name, "", tx)
    }

    /// The inputs that are secrets: a name that says so, with a literal
    /// default.
    pub fn finish(&self, tx: &mut Tx) {
        let names: BTreeMap<String, String> = tx
            .names
            .iter()
            .map(|(orig, native)| (native.clone(), orig.clone()))
            .collect();
        for v in &mut tx.vars {
            if v.kind != VarKind::String {
                continue;
            }
            if v.default.is_none() && v.required != Some(true) {
                v.default = Some(String::new());
            }
            if v.default.is_some() {
                v.required = None;
            }
            let literal = v
                .default
                .as_deref()
                .is_some_and(|d| !d.is_empty() && !d.contains("${"));
            if literal && names.get(&v.name).is_some_and(|o| secretish(o)) {
                v.secret = Some(true);
            }
        }
    }
}

/// The `}` closing the `{` at `open` (defaults may nest `${...}`).
fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (j, c) in s[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + j);
                }
            }
            _ => {}
        }
    }
    None
}
