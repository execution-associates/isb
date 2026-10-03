//! An app's environment as `.env` text, the way Dokploy's environment tab
//! edits it: `KEY=value` lines, comments and blank lines kept in place.
//!
//! A value is plain text, or a reference to a secret in the org's store,
//! written `KEY=${{secret.NAME}}`. A reference is shown as that reference,
//! never as the value it points at.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One variable's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    Plain(String),
    /// `{secret: NAME}`: a secret in the org's store, delivered as the
    /// variable (`environment: {KEY: {secret: ...}}` in the rendered stack).
    Secret {
        secret: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// A comment or blank line, verbatim.
    Text(String),
    Var(String, EnvValue),
}

/// An ordered `.env` file. Serialized as its text; deserialized from text
/// or from a `{KEY: value | {secret: NAME}}` map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvFile {
    lines: Vec<Line>,
}

const SECRET_OPEN: &str = "${{secret.";
const SECRET_CLOSE: &str = "}}";

pub fn validate_key(k: &str) -> Result<()> {
    let ok = !k.is_empty()
        && k.len() <= 256
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "environment variable {k:?}: letters, digits and _, not starting with a digit"
        )))
    }
}

impl EnvFile {
    /// Parse `.env` text. `export KEY=v` is accepted; a later duplicate
    /// replaces the earlier value in place.
    #[expect(
        clippy::excessive_nesting,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    pub fn parse(text: &str) -> Result<EnvFile> {
        let mut f = EnvFile::default();
        let mut lines = text.lines().enumerate().peekable();
        while let Some((n, raw)) = lines.next() {
            let t = raw.trim();
            if t.is_empty() || t.starts_with('#') {
                f.lines.push(Line::Text(raw.trim_end().to_string()));
                continue;
            }
            let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
            let (k, v) = t
                .split_once('=')
                .ok_or_else(|| Error::invalid(format!("line {}: expected KEY=value", n + 1)))?;
            let k = k.trim();
            validate_key(k).map_err(|e| Error::invalid(format!("line {}: {e}", n + 1)))?;
            let v = v.trim_start();
            let value = if let Some(rest) = v.strip_prefix('"') {
                // Double quotes: escapes, and may span lines.
                let mut s = rest.to_string();
                loop {
                    if let Some(end) = closing_quote(&s) {
                        let tail = s[end + 1..].trim();
                        if !(tail.is_empty() || tail.starts_with('#')) {
                            return Err(Error::invalid(format!(
                                "line {}: text after the closing quote",
                                n + 1
                            )));
                        }
                        break EnvValue::Plain(unescape(&s[..end]));
                    }
                    match lines.next() {
                        Some((_, more)) => {
                            s.push('\n');
                            s.push_str(more);
                        }
                        None => {
                            return Err(Error::invalid(format!(
                                "line {}: unterminated quote",
                                n + 1
                            )));
                        }
                    }
                }
            } else if let Some(rest) = v.strip_prefix('\'') {
                let end = rest
                    .find('\'')
                    .ok_or_else(|| Error::invalid(format!("line {}: unterminated quote", n + 1)))?;
                EnvValue::Plain(rest[..end].to_string())
            } else {
                // Unquoted: a ` #` starts a comment, as in docker compose.
                let v = match v.find(" #") {
                    Some(i) => &v[..i],
                    None => v,
                }
                .trim_end();
                match v
                    .strip_prefix(SECRET_OPEN)
                    .and_then(|r| r.strip_suffix(SECRET_CLOSE))
                {
                    Some(name) => {
                        crate::secrets::validate_name(name)
                            .map_err(|e| Error::invalid(format!("line {}: {e}", n + 1)))?;
                        EnvValue::Secret {
                            secret: name.to_string(),
                        }
                    }
                    None => EnvValue::Plain(v.to_string()),
                }
            };
            f.set(k, value);
        }
        // Trailing blank lines are noise from editors.
        while matches!(f.lines.last(), Some(Line::Text(t)) if t.is_empty()) {
            f.lines.pop();
        }
        Ok(f)
    }

    /// The text: comments kept, values quoted where they need it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Text(t) => out.push_str(t),
                Line::Var(k, v) => {
                    out.push_str(k);
                    out.push('=');
                    match v {
                        EnvValue::Secret { secret } => {
                            out.push_str(SECRET_OPEN);
                            out.push_str(secret);
                            out.push_str(SECRET_CLOSE);
                        }
                        EnvValue::Plain(s) => out.push_str(&quote(s)),
                    }
                }
            }
            out.push('\n');
        }
        out
    }

    /// Set a variable: in place if present, else appended.
    pub fn set(&mut self, key: &str, value: EnvValue) {
        for l in &mut self.lines {
            if let Line::Var(k, v) = l {
                if k == key {
                    *v = value;
                    return;
                }
            }
        }
        self.lines.push(Line::Var(key.to_string(), value));
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.lines.len();
        self.lines
            .retain(|l| !matches!(l, Line::Var(k, _) if k == key));
        before != self.lines.len()
    }

    pub fn get(&self, key: &str) -> Option<&EnvValue> {
        self.vars().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    pub fn vars(&self) -> impl Iterator<Item = (&str, &EnvValue)> {
        self.lines.iter().filter_map(|l| match l {
            Line::Var(k, v) => Some((k.as_str(), v)),
            Line::Text(_) => None,
        })
    }

    pub fn to_map(&self) -> BTreeMap<String, EnvValue> {
        self.vars()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    pub fn from_map(m: &BTreeMap<String, EnvValue>) -> Result<EnvFile> {
        let mut f = EnvFile::default();
        for (k, v) in m {
            validate_key(k)?;
            if let EnvValue::Secret { secret } = v {
                crate::secrets::validate_name(secret)?;
            }
            f.set(k, v.clone());
        }
        Ok(f)
    }

    /// The names of the store secrets it references.
    pub fn secret_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .vars()
            .filter_map(|(_, v)| match v {
                EnvValue::Secret { secret } => Some(secret.clone()),
                EnvValue::Plain(_) => None,
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// The index of the first unescaped `"`.
fn closing_quote(s: &str) -> Option<usize> {
    let mut esc = false;
    for (i, c) in s.char_indices() {
        match c {
            _ if esc => esc = false,
            '\\' => esc = true,
            '"' => return Some(i),
            _ => {}
        }
    }
    None
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

fn quote(s: &str) -> String {
    let bare = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:@%+,=-".contains(&b))
        && !s.starts_with(SECRET_OPEN);
    if bare {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Serialize for EnvFile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.render())
    }
}

impl<'de> Deserialize<'de> for EnvFile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Map(BTreeMap<String, EnvValue>),
        }
        match Repr::deserialize(d)? {
            Repr::Text(t) => EnvFile::parse(&t),
            Repr::Map(m) => EnvFile::from_map(&m),
        }
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_comments_and_order() {
        let text = "# database\nDB_HOST=db.shop\nexport PORT=8080\n\n# keys\nTOKEN=${{secret.api_token}}\nGREETING=\"hello world\"\nQ='single # kept'\nINLINE=x # dropped\nMULTI=\"a\nb\"\nEMPTY=\n";
        let f = EnvFile::parse(text).unwrap();
        assert_eq!(
            f.get("TOKEN"),
            Some(&EnvValue::Secret {
                secret: "api_token".into()
            })
        );
        assert_eq!(f.get("PORT"), Some(&EnvValue::Plain("8080".into())));
        assert_eq!(f.get("Q"), Some(&EnvValue::Plain("single # kept".into())));
        assert_eq!(f.get("INLINE"), Some(&EnvValue::Plain("x".into())));
        assert_eq!(f.get("MULTI"), Some(&EnvValue::Plain("a\nb".into())));
        assert_eq!(f.get("EMPTY"), Some(&EnvValue::Plain("".into())));
        let out = f.render();
        assert_eq!(
            out,
            "# database\nDB_HOST=db.shop\nPORT=8080\n\n# keys\nTOKEN=${{secret.api_token}}\nGREETING=\"hello world\"\nQ=\"single # kept\"\nINLINE=x\nMULTI=\"a\\nb\"\nEMPTY=\"\"\n"
        );
        // The rendered text parses back to the same file.
        assert_eq!(EnvFile::parse(&out).unwrap(), f);
        assert_eq!(f.secret_names(), ["api_token"]);
    }

    #[test]
    fn edits_in_place() {
        let mut f = EnvFile::parse("A=1\n# c\nB=2\n").unwrap();
        f.set("A", EnvValue::Plain("9".into()));
        f.set("C", EnvValue::Plain("3".into()));
        assert!(f.remove("B"));
        assert_eq!(f.render(), "A=9\n# c\nC=3\n");
        // A duplicate key: the later value, at the first position.
        let g = EnvFile::parse("A=1\nB=2\nA=3\n").unwrap();
        assert_eq!(g.render(), "A=3\nB=2\n");
    }

    #[test]
    fn refuses_bad_input() {
        assert!(EnvFile::parse("no equals\n").is_err());
        assert!(EnvFile::parse("1A=x\n").is_err());
        assert!(EnvFile::parse("A-B=x\n").is_err());
        assert!(EnvFile::parse("A=\"open\n").is_err());
        assert!(EnvFile::parse("A=\"x\" y\n").is_err());
        assert!(EnvFile::parse("A=${{secret.bad name}}\n").is_err());
    }

    #[test]
    fn serde_forms() {
        let f: EnvFile = serde_json::from_value(serde_json::json!({
            "A": "1", "T": {"secret": "tok"}
        }))
        .unwrap();
        assert_eq!(f.render(), "A=1\nT=${{secret.tok}}\n");
        let g: EnvFile = serde_json::from_value(serde_json::json!("A=1\n")).unwrap();
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!("A=1\n")
        );
        // A plain value that looks like a reference stays plain through a
        // round trip.
        let mut h = EnvFile::default();
        h.set("X", EnvValue::Plain("${{secret.x}}".into()));
        assert_eq!(EnvFile::parse(&h.render()).unwrap(), h);
    }
}
