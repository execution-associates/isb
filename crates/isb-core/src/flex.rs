//! Lenient scalar deserializers.
//!
//! Interpolation runs on the YAML tree before typing, so `cpus: "${CPUS:-8}"`
//! reaches serde as the string `"8"`. These accept either the native scalar or a
//! string that parses as one, the way compose does.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, de::Error as _};

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum BoolOrString {
    Bool(bool),
    String(String),
}

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum IntOrString {
    Int(u64),
    String(String),
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

pub(crate) fn bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    match BoolOrString::deserialize(d)? {
        BoolOrString::Bool(b) => Ok(b),
        BoolOrString::String(s) => {
            parse_bool(&s).ok_or_else(|| D::Error::custom(format!("expected a boolean, got {s:?}")))
        }
    }
}

pub(crate) fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    bool(d).map(Some)
}

/// A required string that may be written as a number (`connect: 5173`).
pub(crate) fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match IntOrString::deserialize(d)? {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s,
    })
}

pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Some(match IntOrString::deserialize(d)? {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s,
    }))
}

/// A scalar (string, number or boolean) read as a string.
#[derive(serde::Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum Scalar {
    String(String),
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl Scalar {
    pub(crate) fn into_string(self) -> String {
        match self {
            Scalar::String(s) => s,
            Scalar::Bool(b) => b.to_string(),
            Scalar::Int(i) => i.to_string(),
            Scalar::Float(f) => f.to_string(),
        }
    }
}

/// A string map whose values may be written as unquoted scalars
/// (`env: {DEBUG: 1}`, `raw_config: {security.nesting: true}`).
pub(crate) fn string_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<String, String>, D::Error> {
    let m = std::collections::BTreeMap::<String, Scalar>::deserialize(d)?;
    Ok(m.into_iter().map(|(k, v)| (k, v.into_string())).collect())
}

/// One `environment` value: a scalar, or a top-level secret delivered as the
/// variable.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum EnvValue {
    Scalar(Scalar),
    Secret {
        /// A top-level secret's key.
        secret: String,
    },
}

/// An environment: a map (a value may be `{secret: NAME}`), or docker's
/// list of `KEY=VALUE` strings.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum EnvMapOrList {
    Map(std::collections::BTreeMap<String, EnvValue>),
    List(Vec<String>),
}

/// A map, or docker's list of `KEY=VALUE` strings.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum MapOrList {
    Map(std::collections::BTreeMap<String, Scalar>),
    List(Vec<String>),
}

fn map_or_list<'de, D: Deserializer<'de>>(
    d: D,
    bare: impl Fn(&str) -> Result<String, String>,
) -> Result<std::collections::BTreeMap<String, String>, D::Error> {
    match MapOrList::deserialize(d)? {
        MapOrList::Map(m) => Ok(m.into_iter().map(|(k, v)| (k, v.into_string())).collect()),
        MapOrList::List(l) => l
            .into_iter()
            .map(|item| match item.split_once('=') {
                Some((k, v)) => Ok((k.to_string(), v.to_string())),
                None => bare(&item).map(|v| (item.clone(), v)),
            })
            .collect::<Result<_, _>>()
            .map_err(D::Error::custom),
    }
}

/// Labels: a map or a list of `KEY=VALUE`; a bare `KEY` is an empty label,
/// as in docker.
pub(crate) fn string_map_or_list<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<String, String>, D::Error> {
    map_or_list(d, |_| Ok(String::new()))
}

/// An environment: a map or a list of `KEY=VALUE`. A compose file resolves a
/// bare `KEY` from the environment before this sees it, as docker does; here,
/// with nothing to resolve it against, it is an error.
pub(crate) fn env_map_or_list<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<String, String>, D::Error> {
    map_or_list(d, |k| {
        Err(format!(
            "environment entry {k:?} has no value: write {k}=VALUE"
        ))
    })
}

/// A command: argv, or a string split the way a shell splits words.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum Command {
    String(String),
    Argv(Vec<Scalar>),
}

pub(crate) fn opt_command<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<String>>, D::Error> {
    match Command::deserialize(d)? {
        Command::Argv(v) => Ok(Some(v.into_iter().map(Scalar::into_string).collect())),
        Command::String(s) => split_words(&s).map(Some).map_err(D::Error::custom),
    }
}

/// Split a command line into words like a POSIX shell, without expanding
/// anything: whitespace separates, quotes group, backslash escapes. Docker
/// splits a string `command` the same way.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err(format!("unterminated ' in {s:?}")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err(format!("unterminated \" in {s:?}")),
                        },
                        Some(c) => cur.push(c),
                        None => return Err(format!("unterminated \" in {s:?}")),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(c) => cur.push(c),
                    None => return Err(format!("trailing \\ in {s:?}")),
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    if words.is_empty() {
        return Err("command is empty".into());
    }
    Ok(words)
}

pub(crate) fn string_map_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>, D::Error>
{
    let m = std::collections::BTreeMap::<String, std::collections::BTreeMap<String, Scalar>>::deserialize(d)?;
    Ok(m.into_iter()
        .map(|(k, v)| {
            (
                k,
                v.into_iter().map(|(a, b)| (a, b.into_string())).collect(),
            )
        })
        .collect())
}

/// Parse `90`, `90s`, `5m`, `1h`, `90d`, `1500ms` into a duration.
pub fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => (&s[..i], s[i..].trim()),
        None => (s, "s"),
    };
    let n: f64 = num
        .parse()
        .map_err(|_| format!("invalid duration {s:?} (use e.g. 90s, 5m, 1h)"))?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" | "sec" | "secs" => n,
        "m" | "min" | "mins" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86400.0,
        _ => return Err(format!("invalid duration unit in {s:?} (ms, s, m, h, d)")),
    };
    Ok(std::time::Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_duration("90d").unwrap(),
            Duration::from_secs(90 * 86400)
        );
        assert_eq!(
            parse_duration("1500ms").unwrap(),
            Duration::from_millis(1500)
        );
        assert!(parse_duration("5 parsecs").is_err());
    }

    #[test]
    fn words() {
        assert_eq!(
            split_words(r#"sh -c 'bun install && exec bun run dev'"#).unwrap(),
            ["sh", "-c", "bun install && exec bun run dev"]
        );
        assert_eq!(
            split_words(r#"echo "a \"b\" $X" c\ d ''"#).unwrap(),
            ["echo", r#"a "b" $X"#, "c d", ""]
        );
        assert!(split_words("echo 'oops").is_err());
        assert!(split_words("   ").is_err());
    }

    #[test]
    fn bools() {
        assert_eq!(parse_bool("TRUE"), Some(true));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }
}
