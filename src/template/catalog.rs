//! Where templates come from: the catalog built into the binary, and the
//! catalogs a platform admin adds (a directory on the host or an https
//! URL; isb's own format or Dokploy's).
//!
//! Added catalogs are kept in `<state>/templates/catalogs.json`. What they
//! hold is third-party data: it is parsed, never run, and a Dokploy
//! template is translated strictly ([`super::dokploy`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::Template;
use super::dokploy::{self, Meta, Report};
use crate::error::{Error, Result};

/// The built-in catalog's name.
pub const BUILTIN: &str = "builtin";

/// The templates compiled into isb (`src/template/builtin/`).
const BUILTIN_FILES: &[(&str, &str)] = &[
    ("uptime-kuma", include_str!("builtin/uptime-kuma.yaml")),
    ("plausible", include_str!("builtin/plausible.yaml")),
    ("gitea", include_str!("builtin/gitea.yaml")),
    ("n8n", include_str!("builtin/n8n.yaml")),
    ("ghost", include_str!("builtin/ghost.yaml")),
    ("umami", include_str!("builtin/umami.yaml")),
    ("vaultwarden", include_str!("builtin/vaultwarden.yaml")),
    ("minio", include_str!("builtin/minio.yaml")),
    (
        "postgres-adminer",
        include_str!("builtin/postgres-adminer.yaml"),
    ),
    ("whoami", include_str!("builtin/whoami.yaml")),
];

/// The built-in templates.
pub fn builtin() -> Vec<Template> {
    BUILTIN_FILES
        .iter()
        .filter_map(|(id, text)| match Template::from_yaml(text) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("isb: built-in template {id}: {e}");
                None
            }
        })
        .collect()
}

/// A catalog's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// isb templates: a directory of `*.yaml` (or `<id>/template.yaml`), or
    /// a URL of one YAML/JSON document `{templates: [...]}`.
    Native,
    /// Dokploy's: `meta.json` plus `blueprints/<id>/{docker-compose.yml,
    /// template.toml}`, in a directory (a checkout of Dokploy/templates) or
    /// at a URL (`https://templates.dokploy.com`).
    Dokploy,
}

/// An added catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    /// `[a-z0-9-]`; template refs are `<name>/<id>`.
    pub name: String,
    pub format: Format,
    /// An absolute directory, or an `https://` URL.
    pub location: String,
}

impl CatalogConfig {
    pub fn validate(&self) -> Result<()> {
        if !super::valid_key(&self.name) || self.name == BUILTIN {
            return Err(Error::invalid(format!(
                "catalog name {:?}: [a-z0-9-], starting with a letter, not {BUILTIN}",
                self.name
            )));
        }
        if self.location.starts_with("https://") {
            return Ok(());
        }
        let p = Path::new(&self.location);
        if !p.is_absolute() {
            return Err(Error::invalid(
                "a catalog's location is an https:// URL or an absolute directory",
            ));
        }
        if !p.is_dir() {
            return Err(Error::invalid(format!(
                "{} is not a directory",
                self.location
            )));
        }
        Ok(())
    }

    fn is_url(&self) -> bool {
        self.location.starts_with("https://")
    }
}

/// A template as a listing shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// `<catalog>/<id>`: what `template_get` and `template_deploy` take.
    #[serde(rename = "ref")]
    pub reference: String,
    pub catalog: String,
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    pub tags: Vec<String>,
    pub links: BTreeMap<String, String>,
    pub format: Format,
}

impl Summary {
    fn of(catalog: &str, t: &Template, format: Format) -> Summary {
        Summary {
            reference: format!("{catalog}/{}", t.id),
            catalog: catalog.into(),
            id: t.id.clone(),
            name: t.name.clone(),
            description: t.description.clone(),
            version: t.version.clone(),
            logo: t.logo.clone(),
            tags: t.tags.clone(),
            links: t.links.clone(),
            format,
        }
    }

    /// Does it match a search: every word in the name, id, description or
    /// tags.
    pub fn matches(&self, query: &str, tag: Option<&str>) -> bool {
        if let Some(t) = tag {
            if !self.tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                return false;
            }
        }
        let hay = format!(
            "{} {} {} {}",
            self.id,
            self.name,
            self.description,
            self.tags.join(" ")
        )
        .to_ascii_lowercase();
        query
            .split_whitespace()
            .all(|w| hay.contains(&w.to_ascii_lowercase()))
    }
}

/// A template, ready to plan, with how it was obtained.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub summary: Summary,
    /// `None` when a Dokploy template was refused.
    pub template: Option<Template>,
    /// For a Dokploy template: how the translation went.
    pub report: Option<Report>,
}

#[derive(Debug, Clone)]
enum Entry {
    Native(Template),
    /// A Dokploy blueprint: its metadata and where its files are (a
    /// directory, or a URL prefix).
    Dokploy {
        meta: Meta,
        base: String,
    },
}

/// Reads a URL (https only, bounded); a test can replace it.
pub type FetchFn = Arc<dyn Fn(&str) -> Result<Vec<u8>> + Send + Sync>;

const CACHE_FOR: Duration = Duration::from_secs(600);
const MAX_BODY: u64 = 4 << 20;

/// A value and when it was read.
type Cached<T> = (Instant, Arc<T>);

/// The catalogs of one daemon.
pub struct Catalogs {
    file: PathBuf,
    fetch: FetchFn,
    builtin: Vec<Template>,
    cache: Mutex<BTreeMap<String, Cached<Vec<Entry>>>>,
    files: Mutex<BTreeMap<String, Cached<Vec<u8>>>>,
}

/// GET an https URL, at most 4 MiB, within 30 s.
pub fn http_fetch(url: &str) -> Result<Vec<u8>> {
    if !url.starts_with("https://") {
        return Err(Error::invalid(format!(
            "{url}: catalogs are fetched over https only"
        )));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .max_redirects(3)
        .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .call()
        .map_err(|e| Error::invalid(format!("GET {url}: {e}")))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::invalid(format!("GET {url}: HTTP {status}")));
    }
    resp.body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .map_err(|e| Error::invalid(format!("GET {url}: {e}")))
}

/// A Dokploy template id: letters, digits, `.`, `_`, `-` (it names a
/// directory and a URL path).
fn dokploy_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Catalogs {
    pub fn new(state: &Path) -> Catalogs {
        Catalogs::with_fetch(state, Arc::new(http_fetch))
    }

    pub fn with_fetch(state: &Path, fetch: FetchFn) -> Catalogs {
        Catalogs {
            file: state.join("templates").join("catalogs.json"),
            fetch,
            builtin: builtin(),
            cache: Mutex::new(BTreeMap::new()),
            files: Mutex::new(BTreeMap::new()),
        }
    }

    /// The added catalogs.
    pub fn configs(&self) -> Result<Vec<CatalogConfig>> {
        match std::fs::read(&self.file) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, c: &[CatalogConfig]) -> Result<()> {
        crate::app::write_atomic(&self.file, &serde_json::to_vec_pretty(c)?)?;
        self.cache.lock().unwrap().clear();
        Ok(())
    }

    /// Add (or replace) a catalog.
    pub fn add(&self, c: CatalogConfig) -> Result<()> {
        c.validate()?;
        let mut all = self.configs()?;
        all.retain(|x| x.name != c.name);
        all.push(c);
        self.save(&all)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let mut all = self.configs()?;
        let n = all.len();
        all.retain(|x| x.name != name);
        if all.len() == n {
            return Err(Error::NotFound(format!("catalog {name}")));
        }
        self.save(&all)
    }

    fn read(&self, cfg: &CatalogConfig, rel: &str) -> Result<Arc<Vec<u8>>> {
        if cfg.is_url() {
            let url = format!("{}/{rel}", cfg.location.trim_end_matches('/'));
            if let Some((at, b)) = self.files.lock().unwrap().get(&url) {
                if at.elapsed() < CACHE_FOR {
                    return Ok(b.clone());
                }
            }
            let b = Arc::new((self.fetch)(&url)?);
            self.files
                .lock()
                .unwrap()
                .insert(url, (Instant::now(), b.clone()));
            Ok(b)
        } else {
            let p = Path::new(&cfg.location).join(rel);
            let meta = std::fs::metadata(&p)
                .map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?;
            if meta.len() > MAX_BODY {
                return Err(Error::invalid(format!("{}: over 4 MiB", p.display())));
            }
            Ok(Arc::new(std::fs::read(&p)?))
        }
    }

    /// Where a Dokploy catalog's blueprints are, relative to its location.
    fn blueprints(cfg: &CatalogConfig) -> &'static str {
        if cfg.is_url() || Path::new(&cfg.location).join("blueprints").is_dir() {
            "blueprints/"
        } else {
            ""
        }
    }

    fn load(&self, cfg: &CatalogConfig) -> Result<Arc<Vec<Entry>>> {
        if let Some((at, e)) = self.cache.lock().unwrap().get(&cfg.name) {
            if at.elapsed() < CACHE_FOR {
                return Ok(e.clone());
            }
        }
        let entries = match cfg.format {
            Format::Native => self.load_native(cfg)?,
            Format::Dokploy => self.load_dokploy(cfg)?,
        };
        let e = Arc::new(entries);
        self.cache
            .lock()
            .unwrap()
            .insert(cfg.name.clone(), (Instant::now(), e.clone()));
        Ok(e)
    }

    fn load_native(&self, cfg: &CatalogConfig) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        if cfg.is_url() {
            let b = self.read(cfg, "")?;
            #[derive(Deserialize)]
            struct Doc {
                templates: Vec<serde_json::Value>,
            }
            let v: serde_json::Value = serde_yaml_ng::from_slice(&b)
                .map_err(|e| Error::invalid(format!("catalog {}: {e}", cfg.name)))?;
            let d: Doc = serde_json::from_value(v)
                .map_err(|e| Error::invalid(format!("catalog {}: {e}", cfg.name)))?;
            for t in d.templates {
                match serde_json::from_value::<Template>(t) {
                    Ok(t) if t.validate().is_ok() => out.push(Entry::Native(t)),
                    _ => {}
                }
            }
        } else {
            let dir = Path::new(&cfg.location);
            let mut paths = Vec::new();
            for e in std::fs::read_dir(dir)?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let t = p.join("template.yaml");
                    if t.is_file() {
                        paths.push(t);
                    }
                } else if p.extension().is_some_and(|x| x == "yaml" || x == "yml") {
                    paths.push(p);
                }
            }
            paths.sort();
            for p in paths {
                let text = std::fs::read_to_string(&p)?;
                match Template::from_yaml(&text) {
                    Ok(t) => out.push(Entry::Native(t)),
                    Err(e) => eprintln!("isb serve: catalog {}: {}: {e}", cfg.name, p.display()),
                }
            }
        }
        Ok(out)
    }

    fn load_dokploy(&self, cfg: &CatalogConfig) -> Result<Vec<Entry>> {
        let prefix = Self::blueprints(cfg);
        let metas: Vec<Meta> = match self.read(cfg, "meta.json") {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| Error::invalid(format!("catalog {}: meta.json: {e}", cfg.name)))?,
            Err(e) if cfg.is_url() => return Err(e),
            Err(_) => {
                // A checkout: each blueprint has its own meta.json.
                let root = Path::new(&cfg.location).join(prefix);
                let mut v = Vec::new();
                for e in std::fs::read_dir(&root)?.flatten() {
                    let id = e.file_name().to_string_lossy().to_string();
                    let m = e.path().join("meta.json");
                    let meta = std::fs::read(&m)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Meta>(&b).ok())
                        .unwrap_or(Meta {
                            id: id.clone(),
                            name: id.clone(),
                            ..Default::default()
                        });
                    if e.path().join("docker-compose.yml").is_file() {
                        v.push(Meta { id, ..meta });
                    }
                }
                v.sort_by(|a, b| a.id.cmp(&b.id));
                v
            }
        };
        let mut out = Vec::new();
        for mut m in metas {
            if !dokploy_id_ok(&m.id) {
                continue;
            }
            let base = format!("{prefix}{}", m.id);
            m.logo = match (&m.logo, cfg.is_url()) {
                (Some(l), true) if dokploy_id_ok(l) => {
                    Some(format!("{}/{base}/{l}", cfg.location.trim_end_matches('/')))
                }
                _ => None,
            };
            out.push(Entry::Dokploy { meta: m, base });
        }
        Ok(out)
    }

    /// Every template, with errors from catalogs that could not be read.
    pub fn list(&self) -> (Vec<Summary>, Vec<String>) {
        let mut out: Vec<Summary> = self
            .builtin
            .iter()
            .map(|t| Summary::of(BUILTIN, t, Format::Native))
            .collect();
        let mut errors = Vec::new();
        match self.configs() {
            Ok(cfgs) => {
                for c in cfgs {
                    match self.load(&c) {
                        Ok(entries) => {
                            for e in entries.iter() {
                                out.push(match e {
                                    Entry::Native(t) => Summary::of(&c.name, t, Format::Native),
                                    Entry::Dokploy { meta, .. } => Summary {
                                        reference: format!("{}/{}", c.name, meta.id),
                                        catalog: c.name.clone(),
                                        id: meta.id.clone(),
                                        name: meta.name.clone(),
                                        description: meta.description.clone(),
                                        version: meta.version.clone(),
                                        logo: meta.logo.clone(),
                                        tags: meta.tags.clone(),
                                        links: meta.links.clone(),
                                        format: Format::Dokploy,
                                    },
                                });
                            }
                        }
                        Err(e) => errors.push(format!("catalog {}: {e}", c.name)),
                    }
                }
            }
            Err(e) => errors.push(format!("catalogs: {e}")),
        }
        (out, errors)
    }

    /// A template by `catalog/id`, or a bare id (the built-in catalog
    /// first, then the added ones in order). A Dokploy template is
    /// translated here.
    pub fn get(&self, reference: &str) -> Result<Resolved> {
        let (catalog, id) = match reference.split_once('/') {
            Some((c, i)) => (Some(c), i),
            None => (None, reference),
        };
        if catalog.is_none_or(|c| c == BUILTIN) {
            if let Some(t) = self.builtin.iter().find(|t| t.id == id) {
                return Ok(Resolved {
                    summary: Summary::of(BUILTIN, t, Format::Native),
                    template: Some(t.clone()),
                    report: None,
                });
            }
            if catalog.is_some() {
                return Err(Error::NotFound(format!("template {reference}")));
            }
        }
        for c in self.configs()? {
            if catalog.is_some_and(|x| x != c.name) {
                continue;
            }
            let entries = self.load(&c)?;
            for e in entries.iter() {
                match e {
                    Entry::Native(t) if t.id == id => {
                        return Ok(Resolved {
                            summary: Summary::of(&c.name, t, Format::Native),
                            template: Some(t.clone()),
                            report: None,
                        });
                    }
                    Entry::Dokploy { meta, base } if meta.id == id => {
                        let compose = self.read(&c, &format!("{base}/docker-compose.yml"))?;
                        let toml = self
                            .read(&c, &format!("{base}/template.toml"))
                            .map(|b| String::from_utf8_lossy(&b).into_owned())
                            .unwrap_or_default();
                        let (t, report) =
                            dokploy::translate(meta, &String::from_utf8_lossy(&compose), &toml);
                        let summary = Summary {
                            reference: format!("{}/{}", c.name, meta.id),
                            catalog: c.name.clone(),
                            id: meta.id.clone(),
                            name: meta.name.clone(),
                            description: meta.description.clone(),
                            version: meta.version.clone(),
                            logo: meta.logo.clone(),
                            tags: meta.tags.clone(),
                            links: meta.links.clone(),
                            format: Format::Dokploy,
                        };
                        return Ok(Resolved {
                            summary,
                            template: t,
                            report: Some(report),
                        });
                    }
                    _ => {}
                }
            }
        }
        Err(Error::NotFound(format!("template {reference}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_templates_parse() {
        let b = builtin();
        assert_eq!(
            b.len(),
            BUILTIN_FILES.len(),
            "a built-in template failed to parse"
        );
        for ((id, _), t) in BUILTIN_FILES.iter().zip(&b) {
            assert_eq!(*id, t.id);
            assert!(!t.description.is_empty(), "{id}: description");
        }
    }

    #[test]
    fn catalogs_add_list_get() {
        let dir = tempfile::tempdir().unwrap();
        let cat = dir.path().join("cat");
        std::fs::create_dir_all(cat.join("dokploy/blueprints/hello")).unwrap();
        std::fs::write(
            cat.join("dokploy/blueprints/hello/docker-compose.yml"),
            "services:\n  hello:\n    image: traefik/whoami:v1.10\n    restart: unless-stopped\n    expose: [80]\n",
        )
        .unwrap();
        std::fs::write(
            cat.join("dokploy/blueprints/hello/template.toml"),
            "[variables]\nmain_domain = \"${domain}\"\n\n[[config.domains]]\nserviceName = \"hello\"\nport = 80\nhost = \"${main_domain}\"\n",
        )
        .unwrap();
        std::fs::write(
            cat.join("dokploy/blueprints/hello/meta.json"),
            r#"{"id":"hello","name":"Hello","description":"hi","tags":["test"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(cat.join("native")).unwrap();
        std::fs::write(
            cat.join("native/echo.yaml"),
            "id: echo\nname: Echo\ndescription: an echo server\napps:\n  - name: web\n    image: docker:traefik/whoami\n    port: 80\n",
        )
        .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::<String>::new()));
        let f2 = fetched.clone();
        let c = Catalogs::with_fetch(
            dir.path(),
            Arc::new(move |u: &str| {
                f2.lock().unwrap().push(u.to_string());
                Err(Error::invalid("offline"))
            }),
        );
        assert!(
            c.add(CatalogConfig {
                name: "builtin".into(),
                format: Format::Native,
                location: "/".into()
            })
            .is_err()
        );
        assert!(
            c.add(CatalogConfig {
                name: "x".into(),
                format: Format::Native,
                location: "relative".into()
            })
            .is_err()
        );
        c.add(CatalogConfig {
            name: "dok".into(),
            format: Format::Dokploy,
            location: cat.join("dokploy").display().to_string(),
        })
        .unwrap();
        c.add(CatalogConfig {
            name: "mine".into(),
            format: Format::Native,
            location: cat.join("native").display().to_string(),
        })
        .unwrap();
        c.add(CatalogConfig {
            name: "remote".into(),
            format: Format::Dokploy,
            location: "https://templates.example.com".into(),
        })
        .unwrap();
        let (all, errors) = c.list();
        assert!(all.iter().any(|s| s.reference == "builtin/uptime-kuma"));
        assert!(
            all.iter()
                .any(|s| s.reference == "dok/hello" && s.format == Format::Dokploy)
        );
        assert!(all.iter().any(|s| s.reference == "mine/echo"));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("remote"));
        assert_eq!(
            fetched.lock().unwrap().as_slice(),
            ["https://templates.example.com/meta.json"]
        );
        let r = c.get("dok/hello").unwrap();
        let t = r.template.unwrap();
        assert_eq!(t.apps[0].image, "docker:traefik/whoami:v1.10");
        assert_eq!(r.report.unwrap().status, dokploy::Status::Clean);
        assert!(c.get("echo").unwrap().template.is_some());
        assert!(c.get("uptime-kuma").unwrap().template.is_some());
        assert!(c.get("dok/nope").is_err());
        assert!(
            all.iter()
                .any(|s| s.reference == "mine/echo" && s.matches("echo server", None))
        );
        assert!(!all.iter().any(|s| s.matches("echo nonsense", None)));
        assert!(all.iter().filter(|s| s.matches("", Some("test"))).count() == 1);
        c.remove("remote").unwrap();
        assert!(c.remove("remote").is_err());
        assert_eq!(c.configs().unwrap().len(), 2);
    }
}
