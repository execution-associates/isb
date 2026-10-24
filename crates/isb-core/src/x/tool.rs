//! An installed tool: a CLI living in a sandbox of its own (its home, its
//! login state, its egress list), run from the host through a shim. Its
//! definition is a YAML file, `$XDG_CONFIG_HOME/isb/x/tools/<name>.yaml`,
//! that `isb x install` writes and `isb x update` rebuilds from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::project::CLOSED;
use super::{IMAGE, LABEL_KIND, PREFIX, USER};
use crate::client::Client;
use crate::egress::EgressSpec;
use crate::error::{Error, Result};
use crate::sandbox::Sandbox;
use crate::spec::SandboxSpec;

/// The label naming the tool an instance belongs to.
pub const LABEL_TOOL: &str = "isbx.tool";

/// PATH in a tool env: the usual places installers put programs.
const PATH: &str = "/home/dev/.local/share/mise/shims:/home/dev/.local/bin:/home/dev/.cargo/bin:/home/dev/go/bin:/home/dev/.bun/bin:/usr/local/bin:/usr/bin:/bin";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    /// The instance in use, `x-<name>-<generation>`: an update builds the
    /// next generation beside it and switches only once that works.
    pub instance: String,
    /// How it is installed (`npm:PKG`, `pypi:PKG`, `cargo:CRATE`, `go:MOD`,
    /// `script:URL`, `sh:LINE`).
    pub recipe: String,
    /// For `script:`, the installer's expected SHA-256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Host command name → program in the guest.
    pub bins: BTreeMap<String, String>,
    /// What a run mounts: `project` (the repository or directory it is run
    /// in), `none`, or one absolute host directory, always.
    #[serde(default = "default_mount")]
    pub mount: String,
    /// Hosts it may reach, as in `egress:` (presets allowed).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Secrets it sees only as placeholders, swapped in on the wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<crate::egress::EgressSecretSpec>,
    /// Secrets read on the host for each run (`VAR: op://vault/item/field`)
    /// and passed in that run's environment, for tools that cannot use a
    /// placeholder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_secrets: BTreeMap<String, String>,
    /// Host variables passed to every run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default = "default_image")]
    pub image: String,
    #[serde(default)]
    pub vm: bool,
}

fn default_mount() -> String {
    "project".into()
}

fn default_image() -> String {
    IMAGE.into()
}

/// What a tool's run mounts.
#[derive(Debug, Clone, PartialEq)]
pub enum Mount {
    Project,
    None,
    Dir(PathBuf),
}

/// Tool names double as file and instance names.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "tool name {name:?}: use 1-40 of a-z, 0-9 and -, starting with a letter"
        )))
    }
}

/// `$XDG_CONFIG_HOME/isb/x/tools`.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("isb/x/tools")
}

impl Tool {
    pub fn path(name: &str) -> PathBuf {
        dir().join(format!("{name}.yaml"))
    }

    pub fn load(name: &str) -> Result<Tool> {
        check_name(name)?;
        let p = Tool::path(name);
        let text = std::fs::read_to_string(&p).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::NotFound(format!(
                "tool {name} (install it with `isb x install {name} --from …`)"
            )),
            _ => Error::invalid(format!("{}: {e}", p.display())),
        })?;
        serde_yaml_ng::from_str(&text).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
    }

    /// Every installed tool, by name.
    pub fn all() -> Vec<Tool> {
        let Ok(rd) = std::fs::read_dir(dir()) else {
            return Vec::new();
        };
        let mut out: Vec<Tool> = rd
            .flatten()
            .filter_map(|e| {
                let n = e
                    .file_name()
                    .to_string_lossy()
                    .strip_suffix(".yaml")?
                    .to_string();
                Tool::load(&n).ok()
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The tool whose bins include host command `cmd`, and that bin.
    pub fn owning(cmd: &str) -> Option<(Tool, String)> {
        Tool::all()
            .into_iter()
            .find(|t| t.bins.contains_key(cmd))
            .map(|t| (t, cmd.to_string()))
    }

    pub fn save(&self) -> Result<()> {
        let p = Tool::path(&self.name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let body = serde_yaml_ng::to_string(self)
            .map_err(|e| Error::invalid(format!("tool {}: {e}", self.name)))?;
        let text = format!(
            "# isbx tool {}: `isb x install` wrote this. Edit it, then `isb x update {}`.\n{body}",
            self.name, self.name
        );
        crate::secrets::local::write_atomic(&p, text.as_bytes())
    }

    pub fn remove_file(&self) {
        let _ = std::fs::remove_file(Tool::path(&self.name));
    }

    pub fn mount(&self) -> Result<Mount> {
        Ok(match self.mount.as_str() {
            "project" => Mount::Project,
            "none" => Mount::None,
            d if Path::new(d).is_absolute() => Mount::Dir(PathBuf::from(d)),
            other => {
                return Err(Error::invalid(format!(
                    "tool {}: mount {other:?} is not project, none or an absolute directory",
                    self.name
                )));
            }
        })
    }

    /// The instance name of generation `n`.
    pub fn instance_of(name: &str, n: u32) -> String {
        format!("{PREFIX}{name}-{n}")
    }

    /// The generation after the one in use.
    pub fn next_instance(&self) -> String {
        let n = self
            .instance
            .rsplit('-')
            .next()
            .and_then(|g| g.parse::<u32>().ok())
            .unwrap_or(0);
        Tool::instance_of(&self.name, n + 1)
    }

    /// The egress list: closed but observable, plus what it was allowed.
    pub fn egress(&self) -> EgressSpec {
        let mut allow = vec![CLOSED.to_string()];
        allow.extend(self.allow.iter().cloned());
        EgressSpec {
            allow,
            none: false,
            secrets: self.secrets.clone(),
            dns_all: true,
        }
    }

    /// The sandbox spec of generation `instance`.
    pub fn spec(&self, instance: &str) -> Result<SandboxSpec> {
        let mut volumes = Vec::new();
        if let Mount::Dir(d) = self.mount()? {
            let d = d.display();
            volumes.push(format!("{d}:{d}"));
        }
        let v = json!({
            "container_name": instance,
            "image": self.image,
            "type": if self.vm { "vm" } else { "container" },
            "idmap": "auto",
            "labels": { LABEL_KIND: "tool", LABEL_TOOL: self.name },
            "volumes": volumes,
            "user": USER,
            "ready": super::ready(self.vm),
            "working_dir": "/home/dev",
            "exec": { "env": { "PATH": PATH, "MISE_YES": "1" } },
            "egress": self.egress(),
        });
        serde_json::from_value(v).map_err(|e| Error::invalid(format!("tool {}: {e}", self.name)))
    }
}

/// Device names of a project mount: `xroot-<hash>`, and `xgit-<hash>` for
/// its `.git`, read-only.
fn mount_devices(root: &Path) -> Vec<(String, serde_json::Value)> {
    let d = ring::digest::digest(&ring::digest::SHA256, root.as_os_str().as_encoded_bytes());
    let h: String = d.as_ref()[..5].iter().map(|b| format!("{b:02x}")).collect();
    let r = root.display().to_string();
    let mut out = vec![(
        format!("xroot-{h}"),
        json!({"type": "disk", "source": r, "path": r}),
    )];
    if root.join(".git").is_dir() {
        let g = format!("{r}/.git");
        out.push((
            format!("xgit-{h}"),
            json!({"type": "disk", "source": g, "path": g, "readonly": "true"}),
        ));
    }
    out
}

fn is_mount_device(name: &str) -> bool {
    name.starts_with("xroot-") || name.starts_with("xgit-")
}

impl Tool {
    /// The tool's sandbox, running, ready for a run from project `root`:
    /// with `mount: project`, that root mounted at its own path (its `.git`
    /// read-only) and every other project mount no live run uses detached,
    /// so a run here cannot write into the project of an earlier one. The
    /// caller notes its own run ([`super::live::begin`]) first.
    pub fn ready(&self, client: &Client, root: Option<&Path>) -> Result<Sandbox> {
        let spec = self.spec(&self.instance)?;
        let sb = match Sandbox::get(client, &self.instance) {
            Ok(sb) => sb.with_exec_defaults(spec.exec_defaults()),
            Err(Error::NotFound(_)) => {
                return Err(Error::NotFound(format!(
                    "{}: tool {}'s sandbox is gone; `isb x update {}` rebuilds it",
                    self.instance, self.name, self.name
                )));
            }
            Err(e) => return Err(e),
        };
        sb.start()?;
        if self.mount()? != Mount::Project {
            return Ok(sb);
        }
        let mut want: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let live = super::live::roots(&self.instance).into_iter().flatten();
        for r in live.chain(root.map(Path::to_path_buf)) {
            want.extend(mount_devices(&r));
        }
        let have = sb.info()?.devices;
        let same = have.keys().filter(|k| is_mount_device(k)).eq(want.keys());
        if same {
            return Ok(sb);
        }
        let _lock = crate::lock::NameLock::acquire(
            client.project_name(),
            &self.instance,
            std::time::Duration::from_secs(120),
            &mut |_| {},
        )?;
        crate::sandbox::update_instance(
            client,
            &self.instance,
            "mount the project",
            &mut |_, devices| {
                devices.retain(|k, _| !is_mount_device(k) || want.contains_key(k));
                for (k, v) in &want {
                    devices.insert(k.clone(), v.clone());
                }
                Ok(())
            },
        )?;
        Ok(sb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> Tool {
        Tool {
            name: "sume".into(),
            instance: Tool::instance_of("sume", 1),
            recipe: "sh:curl -fsSL https://sume.example/install.sh | bash".into(),
            sha256: None,
            bins: [("sume".to_string(), "/home/dev/.sume/bin/sume".to_string())].into(),
            mount: "/home/u/sume".into(),
            allow: vec!["*.sume.example".into(), "@github".into()],
            secrets: vec![
                crate::egress::EgressSecretSpec::parse("SUME_API_KEY@api.sume.example").unwrap(),
            ],
            env_secrets: BTreeMap::new(),
            env: vec![],
            image: IMAGE.into(),
            vm: false,
        }
    }

    #[test]
    fn a_project_mount_is_the_root_and_its_git_read_only() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        assert_eq!(mount_devices(r).len(), 1);
        std::fs::create_dir(r.join(".git")).unwrap();
        let d = mount_devices(r);
        assert!(d[0].0.starts_with("xroot-") && d[1].0.starts_with("xgit-"));
        assert_eq!(d[1].1["readonly"], "true");
        assert_eq!(d[0].1["path"], r.display().to_string());
        assert!(is_mount_device(&d[1].0) && !is_mount_device("root"));
    }

    #[test]
    fn names_generations_and_mounts() {
        assert!(check_name("sume").is_ok() && check_name("gh-cli2").is_ok());
        for bad in ["", "Sume", "-x", "a/b", "a.b", &"x".repeat(41)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        let t = tool();
        assert_eq!(t.instance, "x-sume-1");
        assert_eq!(t.next_instance(), "x-sume-2");
        assert_eq!(t.mount().unwrap(), Mount::Dir("/home/u/sume".into()));
        let mut t2 = t.clone();
        t2.mount = "somewhere".into();
        assert!(t2.mount().is_err());
    }

    #[test]
    fn the_definition_round_trips_and_makes_a_closed_spec() {
        let t = tool();
        let y = serde_yaml_ng::to_string(&t).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<Tool>(&y).unwrap(), t);
        let s = t.spec("x-sume-1").unwrap();
        assert_eq!(s.labels.get(LABEL_TOOL).map(String::as_str), Some("sume"));
        let e = s.egress.unwrap();
        assert_eq!(e.allow[0], CLOSED);
        assert!(e.dns_all && e.secrets.len() == 1);
        let y = serde_yaml_ng::to_string(&t.spec("x-sume-1").unwrap()).unwrap();
        assert!(
            y.contains("source: /home/u/sume\n  target: /home/u/sume"),
            "{y}"
        );
    }
}
