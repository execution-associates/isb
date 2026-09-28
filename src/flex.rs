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

pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Some(match IntOrString::deserialize(d)? {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s,
    }))
}

pub(crate) fn opt_u16<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    let n = match IntOrString::deserialize(d)? {
        IntOrString::Int(n) => n,
        IntOrString::String(s) => s
            .trim()
            .parse::<u64>()
            .map_err(|_| D::Error::custom(format!("expected a number, got {s:?}")))?,
    };
    u16::try_from(n)
        .map(Some)
        .map_err(|_| D::Error::custom(format!("{n} is out of range (max 65535)")))
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

/// Parse `90`, `90s`, `5m`, `1h`, `1500ms` into a duration.
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
        _ => return Err(format!("invalid duration unit in {s:?} (ms, s, m, h)")),
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
            parse_duration("1500ms").unwrap(),
            Duration::from_millis(1500)
        );
        assert!(parse_duration("5 parsecs").is_err());
    }

    #[test]
    fn bools() {
        assert_eq!(parse_bool("TRUE"), Some(true));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }
}
