//! Compose-style variable interpolation.
//!
//! Supported: `$VAR`, `${VAR}`, `${VAR:-default}` (unset or empty), `${VAR-default}`
//! (unset only), `${VAR:?message}` / `${VAR?message}` (error), `${VAR:+alt}` /
//! `${VAR+alt}`, and `$$` for a literal `$`. Defaults may themselves contain
//! interpolations. An unset variable with no default is an error, not an empty
//! string: an empty bind path or label is worse than a failed parse.
//!
//! Interpolation runs on string scalars (and mapping keys) of the parsed YAML tree,
//! never on the raw text, so a value cannot inject YAML structure.

use crate::error::{Error, Result};

/// Interpolate `s` using `lookup` for variable values.
pub fn interpolate(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '$' {
            out.push(c);
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('$') => {
                out.push('$');
                i += 2;
            }
            Some('{') => {
                let end = find_close(&chars, i + 2).ok_or_else(|| {
                    Error::Interpolation(format!("unterminated ${{ in {s:?}"))
                })?;
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str(&expand_braced(&inner, lookup)?);
                i = end + 1;
            }
            Some(&n) if n == '_' || n.is_ascii_alphabetic() => {
                let mut j = i + 1;
                while j < chars.len() && (chars[j] == '_' || chars[j].is_ascii_alphanumeric()) {
                    j += 1;
                }
                let name: String = chars[i + 1..j].iter().collect();
                out.push_str(&lookup(&name).ok_or_else(|| unset(&name))?);
                i = j;
            }
            _ => {
                // A lone `$` (end of string, or followed by something that cannot
                // start a name) is literal, as in compose.
                out.push('$');
                i += 1;
            }
        }
    }
    Ok(out)
}

fn unset(name: &str) -> Error {
    Error::Interpolation(format!(
        "variable {name} is not set (use ${{{name}:-default}} to allow that)"
    ))
}

/// Index of the `}` closing a `${` whose body starts at `start`, honouring nesting.
fn find_close(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '$' if chars.get(i + 1) == Some(&'{') => {
                depth += 1;
                i += 2;
                continue;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn expand_braced(inner: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let name_end = inner
        .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(Error::Interpolation(format!(
            "invalid variable name in ${{{inner}}}"
        )));
    }
    let rest = &inner[name_end..];
    let val = lookup(name);
    if rest.is_empty() {
        return val.ok_or_else(|| unset(name));
    }
    let (op, arg) = if let Some(a) = rest.strip_prefix(":-") {
        (":-", a)
    } else if let Some(a) = rest.strip_prefix(":?") {
        (":?", a)
    } else if let Some(a) = rest.strip_prefix(":+") {
        (":+", a)
    } else if let Some(a) = rest.strip_prefix('-') {
        ("-", a)
    } else if let Some(a) = rest.strip_prefix('?') {
        ("?", a)
    } else if let Some(a) = rest.strip_prefix('+') {
        ("+", a)
    } else {
        return Err(Error::Interpolation(format!(
            "unsupported expansion ${{{inner}}}"
        )));
    };
    let set_nonempty = val.as_deref().is_some_and(|v| !v.is_empty());
    let is_set = val.is_some();
    match op {
        ":-" if set_nonempty => Ok(val.unwrap()),
        "-" if is_set => Ok(val.unwrap()),
        ":-" | "-" => interpolate(arg, lookup),
        ":?" if set_nonempty => Ok(val.unwrap()),
        "?" if is_set => Ok(val.unwrap()),
        ":?" | "?" => {
            let msg = interpolate(arg, lookup)?;
            Err(Error::Interpolation(if msg.is_empty() {
                format!("{name} is required")
            } else {
                format!("{name}: {msg}")
            }))
        }
        ":+" if set_nonempty => interpolate(arg, lookup),
        "+" if is_set => interpolate(arg, lookup),
        _ => Ok(String::new()),
    }
}

/// Interpolate every string scalar and mapping key in a YAML tree, in place.
pub fn interpolate_yaml(
    v: &mut serde_yaml_ng::Value,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<()> {
    use serde_yaml_ng::Value;
    match v {
        Value::String(s) => *s = interpolate(s, lookup)?,
        Value::Sequence(seq) => {
            for item in seq {
                interpolate_yaml(item, lookup)?;
            }
        }
        Value::Mapping(map) => {
            let old = std::mem::take(map);
            for (mut k, mut val) in old {
                interpolate_yaml(&mut k, lookup)?;
                interpolate_yaml(&mut val, lookup)?;
                map.insert(k, val);
            }
        }
        Value::Tagged(t) => interpolate_yaml(&mut t.value, lookup)?,
        _ => {}
    }
    Ok(())
}

/// Parse a dotenv-style file: `KEY=VALUE` lines, `#` comments, optional `export `,
/// optional single or double quotes around the value. No expansion.
pub fn parse_env_file(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (k, v) = line.split_once('=').ok_or_else(|| {
            Error::Invalid(format!("env file line {}: expected KEY=VALUE", n + 1))
        })?;
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v
        };
        out.push((k.trim().to_string(), v.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env() -> HashMap<&'static str, &'static str> {
        HashMap::from([("NAME", "web"), ("EMPTY", ""), ("PORT", "5173")])
    }

    fn run(s: &str) -> Result<String> {
        let e = env();
        interpolate(s, &|k| e.get(k).map(|v| v.to_string()))
    }

    #[test]
    fn plain_and_braced() {
        assert_eq!(run("${NAME}").unwrap(), "web");
        assert_eq!(run("$NAME-x").unwrap(), "web-x");
        assert_eq!(run("tcp:1.2.3.4:${PORT}").unwrap(), "tcp:1.2.3.4:5173");
        assert_eq!(run("no vars").unwrap(), "no vars");
    }

    #[test]
    fn defaults() {
        assert_eq!(run("${MISSING:-d}").unwrap(), "d");
        assert_eq!(run("${EMPTY:-d}").unwrap(), "d");
        assert_eq!(run("${EMPTY-d}").unwrap(), "");
        assert_eq!(run("${MISSING-d}").unwrap(), "d");
        assert_eq!(run("${MISSING:-}").unwrap(), "");
        assert_eq!(run("${MISSING:-${NAME}}").unwrap(), "web");
        assert_eq!(run("${MISSING:-a-${PORT}-b}").unwrap(), "a-5173-b");
    }

    #[test]
    fn alternates() {
        assert_eq!(run("${NAME:+yes}").unwrap(), "yes");
        assert_eq!(run("${EMPTY:+yes}").unwrap(), "");
        assert_eq!(run("${EMPTY+yes}").unwrap(), "yes");
        assert_eq!(run("${MISSING+yes}").unwrap(), "");
    }

    #[test]
    fn errors() {
        let e = run("${MISSING}").unwrap_err().to_string();
        assert!(e.contains("MISSING is not set"), "{e}");
        assert!(run("$MISSING").is_err());
        let e = run("${MISSING:?set the thing}").unwrap_err().to_string();
        assert!(e.contains("set the thing"), "{e}");
        assert!(run("${EMPTY:?x}").is_err());
        assert_eq!(run("${EMPTY?x}").unwrap(), "");
        assert!(run("${NAME").is_err());
        assert!(run("${1BAD}").is_err());
        assert!(run("${NAME/x/y}").is_err());
    }

    #[test]
    fn escapes_and_literals() {
        assert_eq!(run("$$NAME").unwrap(), "$NAME");
        assert_eq!(run("cost: 5$").unwrap(), "cost: 5$");
        assert_eq!(run("a $ b").unwrap(), "a $ b");
        assert_eq!(run("$(cmd)").unwrap(), "$(cmd)");
    }

    #[test]
    fn yaml_tree_values_and_keys() {
        let mut v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str("a: ${NAME}\n${NAME}: [x, '$PORT']\nn: 3\n").unwrap();
        let e = env();
        interpolate_yaml(&mut v, &|k| e.get(k).map(|v| v.to_string())).unwrap();
        let s = serde_yaml_ng::to_string(&v).unwrap();
        assert!(s.contains("a: web"), "{s}");
        assert!(s.contains("web:"), "{s}");
        assert!(s.contains("'5173'"), "{s}");
        assert!(s.contains("n: 3"), "{s}");
    }

    #[test]
    fn value_cannot_inject_yaml() {
        let mut v: serde_yaml_ng::Value = serde_yaml_ng::from_str("a: ${EVIL}\n").unwrap();
        interpolate_yaml(&mut v, &|_| Some("x\nb: injected".into())).unwrap();
        let m = v.as_mapping().unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("a").unwrap().as_str().unwrap(), "x\nb: injected");
    }

    #[test]
    fn env_files() {
        let v = parse_env_file("# c\nA=1\nexport B=\"two words\"\nC='x'\n\nD=\n").unwrap();
        assert_eq!(
            v,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "x".into()),
                ("D".into(), "".into())
            ]
        );
        assert!(parse_env_file("nope").is_err());
    }
}
