//! An app as a document, for `kubectl apply`-style editing: the YAML the
//! web UI's editor shows and `app_export` returns, `app_apply` and the
//! editor's Save take, and the diff between two of them.
//!
//! The document is the app's [`AppSpec`] and nothing else: the fields
//! `app_create` takes. Secrets appear by name (`${{secret.NAME}}` in `env`,
//! `secret:` in `files` and git auth), never as values. Applying a document
//! is declarative: what it leaves out goes back to its default, unlike
//! `app_update`, which merges.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use super::{App, AppSpec, Apps};
use crate::error::{Error, Result};
use crate::org::OrgId;

/// Fields `app_get` adds to an app's settings (state, not settings). A
/// document that carries them, as one pasted from `app_get` does, has them
/// ignored.
const READ_ONLY: &[&str] = &[
    "stack",
    "service_name",
    "current_deployment",
    "created_at",
    "updated_at",
    "webhook",
    "domains_served",
    "env_vars",
    "ingress_enabled",
    "connection",
];

/// Something wrong with a document, where it can be placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// 1-based.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub message: String,
}

impl Problem {
    pub fn new(message: impl Into<String>) -> Problem {
        Problem {
            line: None,
            column: None,
            message: message.into(),
        }
    }

    /// `line 3: message`, for an error string.
    pub fn render(&self) -> String {
        match self.line {
            Some(l) => format!("line {l}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

/// What applying a document does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Created,
    Updated,
    Unchanged,
}

/// What applying a document would do, worked out without doing it.
#[derive(Debug, Clone)]
pub struct Plan {
    pub action: Action,
    /// The settings the document describes, normalized.
    pub spec: AppSpec,
    /// The app's current document; `None` when the app does not exist.
    pub current: Option<String>,
    /// The document as it would be stored.
    pub proposed: String,
    /// A unified diff from `current` to `proposed`.
    pub diff: String,
    /// Top-level fields that differ.
    pub changes: Vec<String>,
    /// What the document takes away from an existing app (see
    /// `super::removals::removals`); empty for a new app.
    pub removals: Vec<String>,
}

/// The order an app's fields are written in.
const ORDER: &[&str] = &[
    "name",
    "project",
    "environment",
    "source",
    "build",
    "env",
    "domains",
    "volumes",
    "ports",
    "replicas",
    "port",
    "healthcheck",
    "resources",
    "command",
    "previews",
    "files",
    "user",
    "working_dir",
];

/// The YAML document of an app's settings. It goes through JSON so that
/// `source: {image: ...}` is a plain mapping, as in the tools' arguments
/// (the YAML serializer would write the enum as a `!image` tag), and the
/// fields come in `ORDER`.
pub fn export_yaml(spec: &AppSpec) -> Result<String> {
    let err = |e: &dyn std::fmt::Display| Error::invalid(format!("app {}: {e}", spec.name));
    let Value::Object(map) = serde_json::to_value(spec).map_err(|e| err(&e))? else {
        return Err(err(&"not a mapping"));
    };
    let mut doc = serde_yaml_ng::Mapping::new();
    let keys = ORDER
        .iter()
        .map(|k| k.to_string())
        .chain(map.keys().filter(|k| !ORDER.contains(&k.as_str())).cloned());
    for k in keys {
        if let Some(v) = map.get(&k) {
            doc.insert(
                serde_yaml_ng::Value::String(k),
                serde_yaml_ng::to_value(v).map_err(|e| err(&e))?,
            );
        }
    }
    serde_yaml_ng::to_string(&doc).map_err(|e| err(&e))
}

/// Parse a document (YAML, which includes JSON) into settings.
pub fn parse(text: &str) -> std::result::Result<AppSpec, Problem> {
    if text.trim().is_empty() {
        return Err(Problem::new("the definition is empty"));
    }
    // The generic parse comes first: its errors are the YAML's own, and it
    // is where read-only fields are dropped.
    let mut v: Value = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(|e| Problem {
            line: Some(e.line()),
            column: Some(e.column()),
            message: clean(&e.to_string()),
        })?
    } else {
        serde_yaml_ng::from_str(text).map_err(|e| yaml_problem(&e))?
    };
    let Value::Object(map) = &mut v else {
        return Err(Problem {
            line: Some(1),
            column: None,
            message: "an app definition is a mapping of fields (name, project, source, ...)".into(),
        });
    };
    for k in READ_ONLY {
        map.remove(*k);
    }
    match serde_json::from_value::<AppSpec>(v.clone()) {
        Ok(s) => Ok(s),
        Err(e) => Err(place(text, &v, &clean(&e.to_string()))),
    }
}

/// Find the line of the field a typed error is about, which serde's own
/// message does not say: try each top-level field alone, among stand-ins
/// for the required ones, and take the first that fails the same way.
fn place(text: &str, v: &Value, message: &str) -> Problem {
    let mut p = Problem::new(message);
    let Value::Object(map) = v else {
        return p;
    };
    let mut first_other = None;
    for (k, val) in map {
        let mut one = serde_json::json!({"name": "x", "project": "x", "source": {"image": "x"}});
        one[k] = val.clone();
        let Err(e) = serde_json::from_value::<AppSpec>(one) else {
            continue;
        };
        let m = clean(&e.to_string());
        if m == message {
            p.line = line_of_key(text, k);
            if !message.contains(&format!("`{k}`")) {
                p.message = format!("{k}: {message}");
            }
            return p;
        }
        first_other.get_or_insert(k);
    }
    // `missing field` and the like are about no one field; an error that
    // only some other wording reproduces still points at its field.
    if !message.starts_with("missing field") {
        p.line = first_other.and_then(|k| line_of_key(text, k));
    }
    if p.line.is_none() {
        p.line = locate_any(text, message).line;
    }
    p
}

fn yaml_problem(e: &serde_yaml_ng::Error) -> Problem {
    let loc = e.location();
    Problem {
        line: loc.as_ref().map(|l| l.line()),
        column: loc.as_ref().map(|l| l.column()),
        message: clean(&e.to_string()),
    }
}

/// An error's text without its trailing ` at line N column M`.
fn clean(s: &str) -> String {
    match s.rfind(" at line ") {
        Some(i)
            if s[i + 9..]
                .chars()
                .all(|c| c.is_ascii_digit() || c == ' ' || c.is_ascii_alphabetic()) =>
        {
            s[..i].to_string()
        }
        _ => s.to_string(),
    }
}

/// The top-level field a validation message is about, by how
/// [`AppSpec::validate`] words them.
fn key_of_message(m: &str) -> Option<&'static str> {
    const PREFIXES: &[(&str, &str)] = &[
        ("replicas", "replicas"),
        ("volume", "volumes"),
        ("file ", "files"),
        ("every domain", "domains"),
        ("domain ", "domains"),
        ("a git source needs", "build"),
        ("an image source", "build"),
        ("app name", "name"),
        ("project name", "project"),
        ("environment name", "environment"),
        ("environment ", "environment"),
        ("project ", "project"),
        ("preview", "previews"),
        ("previews", "previews"),
        ("git ", "source"),
        ("image", "source"),
        ("source", "source"),
        ("a database", "source"),
        ("database", "source"),
        ("an app cannot become", "source"),
        ("an app's name", "name"),
    ];
    PREFIXES
        .iter()
        .find(|(p, _)| m.starts_with(p))
        .map(|(_, k)| *k)
}

/// The 1-based line of a top-level `key:` (or `"key":` in JSON).
pub fn line_of_key(text: &str, key: &str) -> Option<usize> {
    text.lines()
        .position(|l| {
            let t = l.trim_start_matches([' ', '\t', '{', ',']);
            // Top level in YAML: no indent.
            (l.starts_with(key) && l[key.len()..].starts_with(':'))
                || t.strip_prefix('"')
                    .and_then(|r| r.strip_prefix(key))
                    .is_some_and(|r| r.starts_with("\":"))
        })
        .map(|i| i + 1)
}

/// The line a message about a secret or other name points at: the first
/// line that holds `name`.
fn line_of_text(text: &str, name: &str) -> Option<usize> {
    text.lines().position(|l| l.contains(name)).map(|i| i + 1)
}

/// Place a validation `message` in `text`.
pub fn locate(text: &str, message: &str) -> Problem {
    let mut p = Problem::new(message);
    // `secret NAME does not exist ...`
    if let Some(rest) = message.strip_prefix("secret ") {
        if let Some(name) = rest.split_whitespace().next() {
            p.line = line_of_text(text, name);
            return p;
        }
    }
    p.line = key_of_message(message).and_then(|k| line_of_key(text, k));
    if p.line.is_none() {
        return locate_any(text, message);
    }
    p
}

/// Place an error `message` about a YAML `text` by what it quotes: a
/// trailing ` at line N column M`, an ``unknown field `x` ``, or a quoted
/// name (`service "web"`). Any YAML document, not only an app's.
pub fn locate_any(text: &str, message: &str) -> Problem {
    if let Some(i) = message.rfind(" at line ") {
        let mut nums = message[i + 9..]
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<usize>());
        if let (Some(Ok(line)), column) = (nums.next(), nums.next()) {
            return Problem {
                line: Some(line),
                column: column.and_then(Result::ok),
                message: clean(message),
            };
        }
    }
    let message = clean(message);
    let key_line = |name: &str| {
        text.lines()
            .position(|l| {
                let t = l.trim_start().trim_start_matches("- ");
                t.strip_prefix(name)
                    .or_else(|| t.strip_prefix('"').and_then(|r| r.strip_prefix(name)))
                    .is_some_and(|r| r.starts_with(':') || r.starts_with("\":"))
            })
            .map(|i| i + 1)
    };
    let tick = message
        .split_once("unknown field `")
        .and_then(|(_, r)| r.split('`').next());
    let quoted = message.split('"').nth(1).filter(|s| !s.is_empty());
    let line = tick
        .and_then(key_line)
        .or_else(|| quoted.and_then(key_line))
        .or_else(|| quoted.and_then(|n| line_of_text(text, n)));
    Problem {
        line,
        column: None,
        message,
    }
}

/// The RFC 7396 merge patch that turns `old` into `new`.
pub fn merge_diff(old: &Value, new: &Value) -> Value {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut out = serde_json::Map::new();
            for k in o.keys().filter(|k| !n.contains_key(*k)) {
                out.insert(k.clone(), Value::Null);
            }
            for (k, nv) in n {
                match o.get(k) {
                    Some(ov) if ov == nv => {}
                    Some(ov) => {
                        out.insert(k.clone(), merge_diff(ov, nv));
                    }
                    None => {
                        out.insert(k.clone(), nv.clone());
                    }
                }
            }
            Value::Object(out)
        }
        (_, n) => n.clone(),
    }
}

/// The top-level fields on which two settings differ, in document order.
pub fn changed_fields(old: &AppSpec, new: &AppSpec) -> Vec<String> {
    let (o, n) = (
        serde_json::to_value(old).unwrap_or_default(),
        serde_json::to_value(new).unwrap_or_default(),
    );
    let (Value::Object(o), Value::Object(n)) = (o, n) else {
        return Vec::new();
    };
    let keys: BTreeSet<&String> = o.keys().chain(n.keys()).collect();
    let mut out: Vec<String> = keys
        .into_iter()
        .filter(|k| o.get(*k) != n.get(*k))
        .cloned()
        .collect();
    // Document order: the order the fields are written in.
    out.sort_by_key(|k| ORDER.iter().position(|o| o == k).unwrap_or(usize::MAX));
    out
}

/// A unified diff of two texts with three lines of context (`--- current`,
/// `+++ proposed`); empty when they are equal.
pub fn unified_diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a == b {
        return String::new();
    }
    // Trim the common head and tail so the table below is only as big as
    // the edit.
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    // Edit script over the middle: (kind, line) where kind is ' ', '-', '+'.
    let mut ops: Vec<(char, &str)> = a[..head].iter().map(|l| (' ', *l)).collect();
    if ma.len().saturating_mul(mb.len()) > 4_000_000 {
        ops.extend(ma.iter().map(|l| ('-', *l)));
        ops.extend(mb.iter().map(|l| ('+', *l)));
    } else {
        // Longest common subsequence table, filled from the end.
        let (n, m) = (ma.len(), mb.len());
        let mut t = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i][j] = if ma[i] == mb[j] {
                    t[i + 1][j + 1] + 1
                } else {
                    t[i + 1][j].max(t[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if ma[i] == mb[j] {
                ops.push((' ', ma[i]));
                i += 1;
                j += 1;
            } else if t[i + 1][j] >= t[i][j + 1] {
                ops.push(('-', ma[i]));
                i += 1;
            } else {
                ops.push(('+', mb[j]));
                j += 1;
            }
        }
        ops.extend(ma[i..].iter().map(|l| ('-', *l)));
        ops.extend(mb[j..].iter().map(|l| ('+', *l)));
    }
    ops.extend(a[a.len() - tail..].iter().map(|l| (' ', *l)));
    hunks(&ops)
}

fn hunks(ops: &[(char, &str)]) -> String {
    const CONTEXT: usize = 3;
    // Positions (in old, new) before each op, 1-based line numbers.
    let mut pos = Vec::with_capacity(ops.len());
    let (mut ol, mut nl) = (1usize, 1usize);
    for (k, _) in ops {
        pos.push((ol, nl));
        if *k != '+' {
            ol += 1;
        }
        if *k != '-' {
            nl += 1;
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|&i| ops[i].0 != ' ').collect();
    let mut out = String::from("--- current\n+++ proposed\n");
    let mut idx = 0;
    while idx < changed.len() {
        let start = changed[idx].saturating_sub(CONTEXT);
        let mut end = changed[idx];
        // Extend the hunk while the next change is within reach.
        while idx + 1 < changed.len() && changed[idx + 1] <= end + 2 * CONTEXT + 1 {
            idx += 1;
            end = changed[idx];
        }
        idx += 1;
        let end = (end + CONTEXT + 1).min(ops.len());
        let slice = &ops[start..end];
        let old_n = slice.iter().filter(|(k, _)| *k != '+').count();
        let new_n = slice.iter().filter(|(k, _)| *k != '-').count();
        let (os, ns) = pos[start];
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            if old_n == 0 { os - 1 } else { os },
            old_n,
            if new_n == 0 { ns - 1 } else { ns },
            new_n
        ));
        for (k, l) in slice {
            out.push(*k);
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// The rules an update of `old` into `new` must meet, and the
/// normalization it gets (shared with [`Apps::update`]).
pub fn check_update(old: &AppSpec, new: &mut AppSpec) -> Result<()> {
    use super::Source;
    if new.name != old.name || new.project != old.project || new.environment != old.environment {
        return Err(Error::invalid(
            "an app's name, project and environment are fixed; create a new app instead",
        ));
    }
    match (&old.source, &mut new.source) {
        (Source::Database(o), Source::Database(n)) => {
            n.normalize(&old.name);
            if o.engine != n.engine || o.database != n.database || o.user != n.user {
                return Err(Error::invalid(
                    "a database's engine, database and user are fixed (they live in its data volume); restore a backup into a new database instead",
                ));
            }
        }
        (Source::Database(_), _) | (_, Source::Database(_)) => {
            return Err(Error::invalid(
                "an app cannot become a database or stop being one; create a new app",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Work out what applying `text` to the org's apps would do, changing
/// nothing. Every problem found that can be placed in the text carries its
/// line.
pub fn plan(apps: &Apps, org: &OrgId, text: &str) -> std::result::Result<Plan, Problem> {
    let mut spec = parse(text)?;
    if let super::Source::Database(db) = &mut spec.source {
        db.normalize(&spec.name);
    }
    let fail = |e: Error| locate(text, &error_text(&e));
    let existing = match apps.get(org, &spec.name) {
        Ok(a) => Some(a),
        Err(e) if e.is_not_found() => None,
        Err(e) => return Err(fail(e)),
    };
    match &existing {
        Some(a) => check_update(&a.spec, &mut spec).map_err(fail)?,
        None => {
            // What `create` checks before it writes.
            spec.validate().map_err(fail)?;
            let proj = apps
                .project_get(org, &spec.project)
                .map_err(|e| locate(text, &error_text(&e)))?;
            if !proj.environments.contains(&spec.environment) {
                return Err(Problem {
                    line: line_of_key(text, "environment"),
                    column: None,
                    message: format!(
                        "environment {} in project {} (it has {})",
                        spec.environment,
                        spec.project,
                        proj.environments.join(", ")
                    ),
                });
            }
        }
    }
    spec.validate().map_err(fail)?;
    apps.check_spec(org, &spec).map_err(fail)?;
    let proposed = export_yaml(&spec).map_err(fail)?;
    let removals = existing
        .as_ref()
        .map(|a| super::removals::removals(&a.spec, &spec))
        .unwrap_or_default();
    let (action, current, changes) = match &existing {
        None => (
            Action::Created,
            None,
            serde_json::to_value(&spec)
                .ok()
                .and_then(|v| v.as_object().map(|m| m.keys().cloned().collect()))
                .unwrap_or_default(),
        ),
        Some(a) => {
            let changes = changed_fields(&a.spec, &spec);
            let action = if changes.is_empty() {
                Action::Unchanged
            } else {
                Action::Updated
            };
            (action, Some(export_yaml(&a.spec).map_err(fail)?), changes)
        }
    };
    let diff = unified_diff(current.as_deref().unwrap_or(""), &proposed);
    Ok(Plan {
        action,
        spec,
        current,
        proposed,
        diff,
        changes,
        removals,
    })
}

fn error_text(e: &Error) -> String {
    match e {
        Error::Invalid(m) => m.clone(),
        e => e.to_string(),
    }
}

/// Do what `plan` worked out. Returns the app and, for a created app, its
/// webhook secret.
pub fn apply(apps: &Apps, org: &OrgId, plan: &Plan) -> Result<(App, Option<String>)> {
    match plan.action {
        Action::Created => {
            let (app, secret) = apps.create(org, plan.spec.clone())?;
            Ok((app, Some(secret)))
        }
        Action::Unchanged => Ok((apps.get(org, &plan.spec.name)?, None)),
        Action::Updated => {
            let old = apps.get(org, &plan.spec.name)?;
            let patch = merge_diff(
                &serde_json::to_value(&old.spec)?,
                &serde_json::to_value(&plan.spec)?,
            );
            Ok((apps.update(org, &plan.spec.name, &patch)?, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(extra: &str) -> String {
        format!("name: web\nproject: shop\nsource:\n  image: docker:nginx:1.27\n{extra}")
    }

    #[test]
    fn export_then_parse_is_the_same_app() {
        let text = spec("env: |\n  A=1\n  # note\n  B=${{secret.db}}\nreplicas: 3\nport: 80\n");
        let s = parse(&text).unwrap();
        assert_eq!(s.replicas, 3);
        let again = parse(&export_yaml(&s).unwrap()).unwrap();
        assert_eq!(s, again);
        // The env keeps its comment and its secret as a reference.
        let y = export_yaml(&s).unwrap();
        assert!(y.contains("# note"), "{y}");
        assert!(y.contains("${{secret.db}}"), "{y}");
    }

    #[test]
    fn json_is_a_document_too() {
        let s = parse(r#"{"name": "web", "project": "shop", "source": {"image": "docker:nginx"}, "replicas": 2}"#)
            .unwrap();
        assert_eq!(s.replicas, 2);
        let e = parse("{\"name\": \"web\",\n \"nope\": 1}").unwrap_err();
        assert!(e.message.contains("unknown field `nope`"), "{e:?}");
        assert_eq!(e.line, Some(2));
    }

    #[test]
    fn errors_say_which_line() {
        let e = parse(&spec("replicas: many\n")).unwrap_err();
        assert_eq!(e.line, Some(5), "{e:?}");
        assert!(e.message.contains("replicas"), "{e:?}");
        assert!(!e.message.contains("at line"), "{e:?}");
        let e = parse(&spec("bogus: 1\n")).unwrap_err();
        assert_eq!(e.line, Some(5), "{e:?}");
        assert!(e.message.contains("unknown field `bogus`"));
        let e = parse("name: [web\n").unwrap_err();
        assert!(e.line.is_some(), "{e:?}");
        let e = parse("- a\n- b\n").unwrap_err();
        assert!(e.message.contains("mapping"), "{e:?}");
        assert!(parse("  \n").unwrap_err().message.contains("empty"));
    }

    #[test]
    fn what_app_get_adds_is_ignored() {
        let s = parse(&spec(
            "stack: shop-production\nservice_name: web.shop-production\ncurrent_deployment: 4\nenv_vars: {A: '1'}\nwebhook: /x\n",
        ))
        .unwrap();
        assert_eq!(s.name, "web");
    }

    #[test]
    fn messages_are_placed_by_the_field_they_name() {
        let t = spec("replicas: 500\nvolumes: [\"/abs:/x\"]\n");
        assert_eq!(locate(&t, "replicas: at most 100").line, Some(5));
        assert_eq!(locate(&t, "volume \"/abs:/x\": NAME").line, Some(6));
        assert_eq!(locate(&t, "secret db does not exist in org x").line, None);
        let t = spec("env: |\n  A=${{secret.db}}\n");
        assert_eq!(
            locate(&t, "secret db does not exist in org x").line,
            Some(6)
        );
        assert_eq!(line_of_key(&t, "source"), Some(3));
        assert_eq!(line_of_key("{\"a\": 1,\n\"b\": 2}", "b"), Some(2));
    }

    #[test]
    fn any_yaml_error_is_placed_by_what_it_quotes() {
        let t = "services:\n  web:\n    image: x\n    bogus: 1\nsecrets:\n  db: {}\n";
        let p = locate_any(
            t,
            "services.web: unknown field `bogus`, expected one of `image`",
        );
        assert_eq!(p.line, Some(4), "{p:?}");
        let p = locate_any(t, "secret \"db\": needs a source");
        assert_eq!(p.line, Some(6), "{p:?}");
        let p = locate_any(t, "did not find expected key at line 3 column 5");
        assert_eq!((p.line, p.column), (Some(3), Some(5)));
        assert_eq!(p.message, "did not find expected key");
        assert_eq!(locate_any(t, "nothing to quote").line, None);
    }

    #[test]
    fn a_merge_diff_applied_gives_the_new_settings() {
        let old = json!({"a": 1, "b": {"c": 2, "d": 3}, "e": [1, 2], "f": "x"});
        let new = json!({"a": 1, "b": {"c": 9}, "e": [1], "g": true});
        let patch = merge_diff(&old, &new);
        assert_eq!(
            patch,
            json!({"b": {"c": 9, "d": null}, "e": [1], "f": null, "g": true})
        );
        let mut applied = old.clone();
        super::super::merge_patch(&mut applied, &patch);
        assert_eq!(applied, new);
        // Nothing to say when nothing differs.
        assert_eq!(merge_diff(&new, &new), json!({}));
    }

    #[test]
    fn changed_fields_in_document_order() {
        let a = parse(&spec("replicas: 1\nport: 80\n")).unwrap();
        let b = parse(&spec("replicas: 3\nport: 80\nenv: A=1\n")).unwrap();
        assert_eq!(changed_fields(&a, &b), ["env", "replicas"]);
        assert!(changed_fields(&a, &a).is_empty());
    }

    #[test]
    fn unified_diff_shows_the_edit_with_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let new = "a\nb\nc\nd\ne\nf\ng\nH\ni\nj\nk\nl\n";
        let d = unified_diff(old, new);
        assert_eq!(
            d,
            "--- current\n+++ proposed\n@@ -5,7 +5,7 @@\n e\n f\n g\n-h\n+H\n i\n j\n k\n"
        );
        assert_eq!(unified_diff(old, old), "");
        // A pure addition at the start of an empty file.
        assert_eq!(
            unified_diff("", "x\n"),
            "--- current\n+++ proposed\n@@ -0,0 +1,1 @@\n+x\n"
        );
        // Two distant edits are two hunks.
        let long: String = (0..40).map(|i| format!("l{i}\n")).collect();
        let edited = long.replace("l2\n", "L2\n").replace("l35\n", "L35\n");
        assert_eq!(unified_diff(&long, &edited).matches("@@ -").count(), 2);
    }

    use std::sync::Arc;
    use std::time::Duration;

    use crate::client::Client;
    use crate::org::OrgId;
    use crate::secrets::Secrets;
    use crate::stack::Controller;

    /// An `Apps` with no incusd behind it: everything but a deploy works.
    fn apps(dir: &std::path::Path) -> Apps {
        let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
        let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
            dir,
            Arc::new(k),
        )));
        let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
        let store = crate::stack::Store::open(dir).unwrap();
        let ctl = Controller::start(
            client.clone(),
            store,
            Duration::from_secs(60),
            secrets.clone(),
        )
        .unwrap();
        Apps::new(dir, client, ctl, secrets)
    }

    fn shop(ap: &Apps, org: &OrgId) {
        ap.project_create(org, "shop", "", &["production".into(), "staging".into()])
            .unwrap();
    }

    #[test]
    fn apply_creates_then_updates_then_finds_nothing_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let ap = apps(dir.path());
        let org = OrgId::default_org();
        shop(&ap, &org);

        // A new name is a create; a dry run writes nothing.
        let doc = spec("replicas: 2\nport: 80\n");
        let p = plan(&ap, &org, &doc).unwrap();
        assert_eq!(p.action, Action::Created);
        assert!(p.current.is_none());
        assert!(p.diff.contains("+replicas: 2"), "{}", p.diff);
        assert!(ap.get(&org, "web").is_err(), "a plan stores nothing");

        let (app, secret) = apply(&ap, &org, &p).unwrap();
        assert_eq!(app.spec.replicas, 2);
        assert!(secret.is_some_and(|s| !s.is_empty()));

        // The same document again: nothing changes.
        let p = plan(&ap, &org, &doc).unwrap();
        assert_eq!(p.action, Action::Unchanged);
        assert!(p.changes.is_empty() && p.diff.is_empty());

        // The exported document is a fixed point too.
        let exported = export_yaml(&ap.get(&org, "web").unwrap().spec).unwrap();
        assert_eq!(
            plan(&ap, &org, &exported).unwrap().action,
            Action::Unchanged
        );

        // Declarative: leaving `port` and `replicas` out resets them.
        let p = plan(&ap, &org, &spec("env: A=1\n")).unwrap();
        assert_eq!(p.action, Action::Updated);
        assert_eq!(p.changes, ["env", "replicas", "port"]);
        assert!(
            p.diff.contains("-replicas: 2") && p.diff.contains("-port: 80"),
            "{}",
            p.diff
        );
        let (app, secret) = apply(&ap, &org, &p).unwrap();
        assert!(secret.is_none());
        assert_eq!((app.spec.replicas, app.spec.port), (1, None));
        assert_eq!(app.spec.env.render(), "A=1\n");
        assert_eq!(ap.get(&org, "web").unwrap().spec, app.spec);
    }

    #[test]
    fn apply_refuses_what_update_refuses_and_places_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let ap = apps(dir.path());
        let org = OrgId::default_org();
        shop(&ap, &org);
        apply(&ap, &org, &plan(&ap, &org, &spec("")).unwrap()).unwrap();

        // Moving an app is not an update.
        let moved =
            "name: web\nproject: shop\nenvironment: staging\nsource: {image: docker:nginx}\n";
        let e = plan(&ap, &org, moved).unwrap_err();
        assert!(e.message.contains("fixed"), "{e:?}");

        // A project that is not there, an environment that is not there.
        let e = plan(
            &ap,
            &org,
            &spec("").replace("shop", "nope").replace("web", "other"),
        )
        .unwrap_err();
        assert!(e.message.contains("nope"), "{e:?}");
        let e = plan(
            &ap,
            &org,
            "name: db\nproject: shop\nenvironment: qa\nsource: {image: docker:x}\n",
        )
        .unwrap_err();
        assert_eq!(e.line, Some(3), "{e:?}");

        // A spec the validator refuses, a secret that is not there.
        let e = plan(&ap, &org, &spec("replicas: 500\n")).unwrap_err();
        assert_eq!(e.line, Some(5), "{e:?}");
        let e = plan(&ap, &org, &spec("env: |\n  K=${{secret.ghost}}\n")).unwrap_err();
        assert!(e.message.contains("ghost"), "{e:?}");
        assert_eq!(e.line, Some(6), "{e:?}");
        // Nothing of that was stored.
        assert_eq!(ap.get(&org, "web").unwrap().spec.replicas, 1);
    }

    #[test]
    fn plans_carry_removals_only_for_an_existing_app() {
        // Files name org secrets, which must exist; the other fields suffice here.
        let full = "env: |\n  A=1\n  B=2\ndomains:\n  - host: a.example.com\n    https: true\n  - host: b.example.com\nvolumes: [\"data:/data\"]\nports: [\"127.0.0.1:8080:80\"]\nport: 80\nreplicas: 2\nhealthcheck: {test: [\"CMD\", \"true\"]}\ncommand: [\"run\"]\nuser: \"1000\"\n";
        let dir = tempfile::tempdir().unwrap();
        let ap = apps(dir.path());
        let org = OrgId::default_org();
        shop(&ap, &org);

        // Create: nothing to remove.
        let p = plan(&ap, &org, &spec(full)).unwrap();
        assert_eq!(p.action, Action::Created);
        assert!(p.removals.is_empty());
        apply(&ap, &org, &p).unwrap();

        // Unchanged and a pure addition: none.
        assert!(plan(&ap, &org, &spec(full)).unwrap().removals.is_empty());
        let more = format!("{full}working_dir: /srv\n");
        let p = plan(&ap, &org, &spec(&more)).unwrap();
        assert_eq!(p.action, Action::Updated);
        assert!(p.removals.is_empty());

        // A short document: the plan says what it drops, and writes nothing.
        let p = plan(&ap, &org, &spec("replicas: 2\n")).unwrap();
        assert_eq!(p.action, Action::Updated);
        assert!(p.removals.contains(&"domains: a.example.com".to_string()));
        assert!(p.removals.contains(&"env: A".to_string()));
        assert_eq!(ap.get(&org, "web").unwrap().spec.domains.len(), 2);
    }
}
