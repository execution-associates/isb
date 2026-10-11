//! Agent Skills over MCP: the `io.modelcontextprotocol/skills` extension
//! (SEP-2640). Each file of a skill is a resource at `skill://NAME/PATH`;
//! `skills/list` and `skills/get` hand out manifests with digests, and
//! `resources/read` and `resources/directory/read` serve the files.
//!
//! Skills are compiled into the binary, so their digests only change on an
//! upgrade and every caller sees the same ones, whatever its org.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::{INVALID_PARAMS, Registry, RpcResult};
use crate::error::{Error, Result};

const EXTENSION: &str = "io.modelcontextprotocol/skills";
const SCHEME: &str = "skill://";
/// How long a client may treat a listing, entry or file as current.
const TTL_MS: u64 = 300_000;
/// The extension's per-skill limits: what every conforming host accepts.
const MAX_FILES: usize = 512;
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// One skill: a `SKILL.md` and the files beside it.
#[derive(Debug, Clone)]
pub struct Skill {
    name: String,
    /// `SKILL.md`'s frontmatter as JSON, every field the author wrote.
    frontmatter: Value,
    /// (path under the skill root, bytes), `SKILL.md` first.
    files: Vec<(String, Vec<u8>)>,
}

impl Skill {
    /// A skill from its files, by path relative to the skill root. One must
    /// be `SKILL.md`, with `name` and `description` in its frontmatter.
    pub fn new<P, B>(files: impl IntoIterator<Item = (P, B)>) -> Result<Self>
    where
        P: Into<String>,
        B: Into<Vec<u8>>,
    {
        let mut files: Vec<(String, Vec<u8>)> = files
            .into_iter()
            .map(|(p, b)| (p.into(), b.into()))
            .collect();
        for (p, _) in &files {
            if p.is_empty()
                || p.starts_with('/')
                || p.ends_with('/')
                || p.split('/').any(|s| matches!(s, "" | "." | ".."))
            {
                return Err(Error::invalid(format!("invalid skill file path {p:?}")));
            }
        }
        files.sort_by(|a, b| (a.0 != "SKILL.md", &a.0).cmp(&(b.0 != "SKILL.md", &b.0)));
        if files.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(Error::invalid("a skill lists a file twice"));
        }
        let Some((_, md)) = files.first().filter(|(p, _)| p == "SKILL.md") else {
            return Err(Error::invalid("a skill needs a SKILL.md"));
        };
        let frontmatter = frontmatter(md)?;
        let name = frontmatter["name"].as_str().unwrap_or_default().to_string();
        if !valid_name(&name) {
            return Err(Error::invalid(format!(
                "skill name {name:?}: 1-64 of a-z, 0-9 and single hyphens inside"
            )));
        }
        if !frontmatter["description"].is_string() {
            return Err(Error::invalid(format!("skill {name}: no description")));
        }
        let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
        if files.len() > MAX_FILES || bytes > MAX_BYTES {
            return Err(Error::invalid(format!(
                "skill {name}: {} files, {bytes} bytes; hosts only promise {MAX_FILES} and {MAX_BYTES}",
                files.len()
            )));
        }
        Ok(Self {
            name,
            frontmatter,
            files,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn root(&self) -> String {
        format!("{SCHEME}{}", self.name)
    }

    fn uri(&self, path: &str) -> String {
        format!("{}/{path}", self.root())
    }

    /// The `Skill` entry `skills/list` and `skills/get` return.
    fn entry(&self) -> Value {
        let resources: Vec<Value> = self
            .files
            .iter()
            .map(|(p, b)| json!({"uri": self.uri(p), "digest": digest(b), "size": b.len()}))
            .collect();
        json!({
            "uri": self.uri("SKILL.md"),
            "frontmatter": self.frontmatter,
            "resources": resources,
        })
    }

    /// A file's `Resource` metadata. `SKILL.md` carries the skill's name and
    /// description, as the extension asks.
    fn resource(&self, path: &str, bytes: &[u8]) -> Value {
        let mut r = json!({
            "uri": self.uri(path),
            "name": path.rsplit('/').next().unwrap_or(path),
            "mimeType": mime(path, bytes),
            "size": bytes.len(),
        });
        if path == "SKILL.md" {
            r["name"] = json!(self.name);
            r["description"] = self.frontmatter["description"].clone();
        }
        r
    }
}

/// `SKILL.md`'s YAML frontmatter, between the opening `---` and the next.
fn frontmatter(md: &[u8]) -> Result<Value> {
    let bad = || Error::invalid("SKILL.md must open with YAML frontmatter between --- lines");
    let text = std::str::from_utf8(md).map_err(|_| bad())?;
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(bad)?;
    let end = rest
        .match_indices("\n---")
        .map(|(i, _)| i)
        .find(|&i| matches!(rest[i + 4..].chars().next(), None | Some('\n' | '\r')))
        .ok_or_else(bad)?;
    let v: Value = serde_yaml_ng::from_str(&rest[..end])
        .map_err(|e| Error::invalid(format!("SKILL.md frontmatter: {e}")))?;
    if !v.is_object() {
        return Err(bad());
    }
    Ok(v)
}

/// The Agent Skills naming rule.
fn valid_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !n.starts_with('-')
        && !n.ends_with('-')
        && !n.contains("--")
}

fn digest(b: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, b);
    let hex: String = d.as_ref().iter().map(|x| format!("{x:02x}")).collect();
    format!("sha256:{hex}")
}

fn mime(path: &str, bytes: &[u8]) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    match ext {
        "md" => "text/markdown",
        "yaml" | "yml" => "application/yaml",
        "json" => "application/json",
        "sh" => "text/x-shellscript",
        "py" => "text/x-python",
        _ if std::str::from_utf8(bytes).is_ok() => "text/plain",
        _ => "application/octet-stream",
    }
}

impl Registry {
    /// Serve these skills at `skill://NAME/`. Names are unique.
    pub fn skills(mut self, skills: impl IntoIterator<Item = Skill>) -> Result<Self> {
        for s in skills {
            if self.skills.iter().any(|t| t.name == s.name) {
                return Err(Error::invalid(format!("skill {:?} added twice", s.name)));
            }
            self.skills.push(s);
        }
        Ok(self)
    }
}

/// Declare the extension, and the resources it serves through, in
/// `initialize`'s capabilities.
pub(super) fn declare(skills: &[Skill], caps: &mut Value) {
    if !skills.is_empty() {
        caps["resources"] = json!({"listChanged": false});
        caps["extensions"] = json!({EXTENSION: {"directoryRead": true}});
    }
}

/// Answer a skills or resources method, or `None` when `method` is neither
/// or there are no skills (then there are no resources either).
pub(super) fn dispatch(skills: &[Skill], method: &str, params: &Value) -> Option<RpcResult> {
    if skills.is_empty() {
        return None;
    }
    let bad = |m: String| Err((INVALID_PARAMS, m));
    let cached = |mut v: Value| {
        v["resultType"] = json!("complete");
        v["ttlMs"] = json!(TTL_MS);
        v["cacheScope"] = json!("public");
        Ok(v)
    };
    let uri = || params.get("uri").and_then(Value::as_str);
    // A handful of files: nothing is ever paginated, so a cursor is ignored.
    if params.get("cursor").is_some_and(|c| !c.is_string()) {
        return Some(bad("cursor must be a string".into()));
    }
    Some(match method {
        "skills/list" => {
            cached(json!({"skills": skills.iter().map(Skill::entry).collect::<Vec<_>>()}))
        }
        "skills/get" => {
            let Some(u) = uri() else {
                return Some(bad("skills/get needs a uri".into()));
            };
            match skills.iter().find(|s| s.uri("SKILL.md") == u) {
                Some(s) => cached(json!({"skill": s.entry()})),
                None => bad(format!("No skill is served at {u}")),
            }
        }
        "resources/list" => {
            let r: Vec<Value> = skills
                .iter()
                .flat_map(|s| s.files.iter().map(move |(p, b)| s.resource(p, b)))
                .collect();
            cached(json!({"resources": r}))
        }
        "resources/templates/list" => cached(json!({"resourceTemplates": []})),
        "resources/read" => {
            let Some(u) = uri() else {
                return Some(bad("resources/read needs a uri".into()));
            };
            let found = skills
                .iter()
                .find_map(|s| s.files.iter().find(|(p, _)| s.uri(p) == u));
            let Some((p, b)) = found else {
                return Some(bad(format!("no resource at {u}")));
            };
            let mut c = json!({"uri": u, "mimeType": mime(p, b)});
            match std::str::from_utf8(b) {
                Ok(t) => c["text"] = json!(t),
                Err(_) => c["blob"] = json!(STANDARD.encode(b)),
            }
            cached(json!({"contents": [c]}))
        }
        "resources/directory/read" => {
            let Some(u) = uri() else {
                return Some(bad("resources/directory/read needs a uri".into()));
            };
            match directory(skills, u) {
                Some(r) => Ok(json!({"resultType": "complete", "resources": r})),
                None => bad(format!("{u} is not a directory resource")),
            }
        }
        _ => return None,
    })
}

/// The direct children of the directory at `uri` (no trailing slash), or
/// `None` when no skill has a directory there.
fn directory(skills: &[Skill], uri: &str) -> Option<Vec<Value>> {
    for s in skills {
        let root = s.root();
        let dir = if uri == root {
            ""
        } else if let Some(d) = uri.strip_prefix(&root).and_then(|d| d.strip_prefix('/')) {
            d
        } else {
            continue;
        };
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut out: Vec<Value> = Vec::new();
        let mut subdirs: Vec<&str> = Vec::new();
        for (p, b) in &s.files {
            let Some(rest) = p.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                None => out.push(s.resource(p, b)),
                Some((d, _)) if !subdirs.contains(&d) => subdirs.push(d),
                Some(_) => {}
            }
        }
        if out.is_empty() && subdirs.is_empty() {
            return None;
        }
        out.extend(subdirs.into_iter().map(|d| {
            json!({"uri": s.uri(&format!("{prefix}{d}")), "name": d, "mimeType": "inode/directory"})
        }));
        return Some(out);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "---\nname: pdf-processing\ndescription: Extract, fill, and assemble PDF documents\n---\n\n# PDF processing\n\nChoose the matching template from `templates/`.\n";

    fn skill() -> Skill {
        Skill::new([
            (
                "templates/purchase-order.md",
                "# Purchase order\n\nSupplier:\nItems:\n",
            ),
            ("SKILL.md", MD),
            ("templates/invoice.md", "# Invoice\n\nCustomer:\nAmount:\n"),
        ])
        .unwrap()
    }

    fn call(method: &str, params: Value) -> RpcResult {
        dispatch(&[skill()], method, &params).expect("a skills method")
    }

    #[test]
    fn entry_matches_the_spec_example() {
        // The digests and sizes from the extension's own example flow.
        let r = call(
            "skills/get",
            json!({"uri": "skill://pdf-processing/SKILL.md"}),
        )
        .unwrap();
        assert_eq!(r["resultType"], "complete");
        assert_eq!(r["cacheScope"], "public");
        assert_eq!(
            r["skill"],
            json!({
                "uri": "skill://pdf-processing/SKILL.md",
                "frontmatter": {"name": "pdf-processing", "description": "Extract, fill, and assemble PDF documents"},
                "resources": [
                    {"uri": "skill://pdf-processing/SKILL.md", "digest": "sha256:99b737495721155ece826d57521e2d66141ebdc1344a400487481ea2642ab19e", "size": 151},
                    {"uri": "skill://pdf-processing/templates/invoice.md", "digest": "sha256:61f4ea6d2c75fde1b4977219e7e3107d491c3c26aefb6686e84d6281c088d9ee", "size": 29},
                    {"uri": "skill://pdf-processing/templates/purchase-order.md", "digest": "sha256:f2ff774b1737ff3dec81c47946f9976f18a1a9f69dda0a81f22eabd95173c158", "size": 35},
                ],
            })
        );
        let l = call("skills/list", json!({})).unwrap();
        assert_eq!(l["skills"], json!([r["skill"]]));
        assert_eq!(
            call("skills/get", json!({"uri": "skill://nope/SKILL.md"}))
                .unwrap_err()
                .0,
            INVALID_PARAMS
        );
    }

    #[test]
    fn files_read_as_resources() {
        let r = call(
            "resources/read",
            json!({"uri": "skill://pdf-processing/SKILL.md"}),
        )
        .unwrap();
        assert_eq!(
            r["contents"],
            json!([{"uri": "skill://pdf-processing/SKILL.md", "mimeType": "text/markdown", "text": MD}])
        );
        assert_eq!(
            call(
                "resources/read",
                json!({"uri": "skill://pdf-processing/x.md"})
            )
            .unwrap_err()
            .0,
            INVALID_PARAMS
        );
        let l = call("resources/list", json!({})).unwrap();
        let l = l["resources"].as_array().unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[0]["name"], "pdf-processing");
        assert_eq!(
            l[0]["description"],
            "Extract, fill, and assemble PDF documents"
        );
        assert_eq!(l[1]["name"], "invoice.md");
    }

    #[test]
    fn directories_list_direct_children() {
        let root = call(
            "resources/directory/read",
            json!({"uri": "skill://pdf-processing"}),
        )
        .unwrap();
        let names: Vec<_> = root["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["name"].as_str().unwrap(), r["mimeType"].as_str().unwrap()))
            .collect();
        assert_eq!(
            names,
            [
                ("pdf-processing", "text/markdown"),
                ("templates", "inode/directory")
            ]
        );
        let t = call(
            "resources/directory/read",
            json!({"uri": "skill://pdf-processing/templates"}),
        )
        .unwrap();
        assert_eq!(t["resources"].as_array().unwrap().len(), 2);
        for not_dir in [
            "skill://pdf-processing/SKILL.md",
            "skill://pdf-processing/nope",
            "skill://pdf",
        ] {
            assert_eq!(
                call("resources/directory/read", json!({"uri": not_dir}))
                    .unwrap_err()
                    .0,
                INVALID_PARAMS,
                "{not_dir}"
            );
        }
    }

    #[test]
    fn frontmatter_passes_through_verbatim() {
        let md = "---\nname: isb\ndescription: >\n  Two\n  lines.\nlicense: MIT\nmetadata:\n  k: v\n---\nbody\n---\nmore\n";
        let s = Skill::new([("SKILL.md", md)]).unwrap();
        assert_eq!(
            s.entry()["frontmatter"],
            json!({"name": "isb", "description": "Two lines.\n", "license": "MIT", "metadata": {"k": "v"}})
        );
    }

    #[test]
    fn bad_skills_are_refused() {
        let ok = "---\nname: a\ndescription: d\n---\n";
        for files in [
            vec![("README.md", ok)],
            vec![("SKILL.md", "no frontmatter")],
            vec![("SKILL.md", "---\nname: Bad_Name\ndescription: d\n---\n")],
            vec![("SKILL.md", "---\nname: a--b\ndescription: d\n---\n")],
            vec![("SKILL.md", "---\nname: a\n---\n")],
            vec![("SKILL.md", ok), ("../x", "")],
            vec![("SKILL.md", ok), ("a//b", "")],
            vec![("SKILL.md", ok), ("SKILL.md", ok)],
        ] {
            assert!(Skill::new(files.clone()).is_err(), "{files:?}");
        }
    }
}
