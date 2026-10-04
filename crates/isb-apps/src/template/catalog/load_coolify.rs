//! Reading a Coolify catalog: which templates it has and each one's header.
//!
//! A directory is a checkout of coollabsio/coolify (or just its
//! `templates/compose`). A URL is the repository's raw files
//! (`https://raw.githubusercontent.com/coollabsio/coolify/main`): the list
//! of templates is GitHub's directory listing of `templates/compose`, or,
//! when that is unavailable, the repository's generated
//! `templates/service-templates.json`; each template's header is read from
//! its own file, a few at a time.

use super::*;

/// Where a checkout keeps the templates.
const DIR: &str = "templates/compose";

/// How many files are fetched at once.
const PARALLEL: usize = 12;

/// `(id, path relative to the catalog's location)`.
type Files = Vec<(String, String)>;

/// `https://github.com/<owner>/<repo>[/tree/<ref>]` as the raw-files URL of
/// the same place; anything else as it is.
pub(super) fn normalize_location(loc: &str) -> String {
    let Some(rest) = loc
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
    else {
        return loc.to_string();
    };
    let p: Vec<&str> = rest.split('/').collect();
    match p.as_slice() {
        [o, r] => format!("https://raw.githubusercontent.com/{o}/{r}/HEAD"),
        [o, r, "tree", rf] => format!("https://raw.githubusercontent.com/{o}/{r}/{rf}"),
        _ => loc.to_string(),
    }
}

/// GitHub's listing of `templates/compose` for a raw-files URL.
fn github_listing(base: &str) -> Option<String> {
    let rest = base.strip_prefix("https://raw.githubusercontent.com/")?;
    let p: Vec<&str> = rest.split('/').collect();
    let [owner, repo, rf, sub @ ..] = p.as_slice() else {
        return None;
    };
    let sub: String = sub.iter().map(|s| format!("{s}/")).collect();
    Some(format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{sub}{DIR}?ref={rf}"
    ))
}

/// A file name that is a template: `<id>.yaml` or `.yml`.
fn template_id(name: &str) -> Option<String> {
    let id = name
        .strip_suffix(".yaml")
        .or_else(|| name.strip_suffix(".yml"))?;
    dokploy_id_ok(id).then(|| id.to_string())
}

/// `(id, file name)` pairs for the files of a listing.
fn template_files<'a>(names: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    names
        .filter_map(|n| Some((template_id(n)?, n.to_string())))
        .collect()
}

impl Catalogs {
    pub(super) fn load_coolify(&self, cfg: &CatalogConfig) -> Result<Vec<Entry>> {
        let files = if cfg.is_url() {
            self.coolify_remote_files(cfg)?
        } else {
            coolify_local_files(cfg)?
        };
        let logo_base = cfg
            .is_url()
            .then(|| cfg.location.trim_end_matches('/').to_string());
        let one = |(id, rel): &(String, String)| -> Option<Entry> {
            let b = self.read(cfg, rel).ok()?;
            let meta = coolify::meta(id, &String::from_utf8_lossy(&b), logo_base.as_deref());
            (!meta.ignore).then(|| Entry::Coolify {
                meta,
                file: rel.clone(),
            })
        };
        let entries: Vec<Option<Entry>> = if cfg.is_url() {
            let chunk = files.len().div_ceil(PARALLEL).max(1);
            std::thread::scope(|sc| {
                let workers: Vec<_> = files
                    .chunks(chunk)
                    .map(|c| sc.spawn(|| c.iter().map(one).collect::<Vec<_>>()))
                    .collect();
                workers
                    .into_iter()
                    .flat_map(|w| w.join().unwrap_or_default())
                    .collect()
            })
        } else {
            files.iter().map(one).collect()
        };
        let mut out: Vec<Entry> = entries.into_iter().flatten().collect();
        if out.is_empty() && !files.is_empty() {
            return Err(Error::invalid(format!(
                "none of the {} templates could be read",
                files.len()
            )));
        }
        out.sort_by(|a, b| match (a, b) {
            (Entry::Coolify { meta: a, .. }, Entry::Coolify { meta: b, .. }) => a.id.cmp(&b.id),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(out)
    }

    /// The template files at a URL.
    fn coolify_remote_files(&self, cfg: &CatalogConfig) -> Result<Files> {
        let base = cfg.location.trim_end_matches('/');
        let listed = github_listing(base)
            .and_then(|u| self.fetch_cached(&u).ok())
            .and_then(|b| serde_json::from_slice::<Vec<serde_json::Value>>(&b).ok())
            .map(|v| {
                template_files(
                    v.iter()
                        .filter(|e| e["type"] == "file")
                        .filter_map(|e| e["name"].as_str()),
                )
            })
            .filter(|v| !v.is_empty());
        let files = match listed {
            Some(f) => f,
            None => {
                // The index lists most of the templates, not all.
                let b = self.read(cfg, "templates/service-templates.json")?;
                let v: serde_json::Value = serde_json::from_slice(&b).map_err(|e| {
                    Error::invalid(format!("catalog {}: service-templates.json: {e}", cfg.name))
                })?;
                v.as_object()
                    .map(|o| {
                        template_files(
                            o.keys()
                                .map(|k| format!("{k}.yaml"))
                                .collect::<Vec<_>>()
                                .iter()
                                .map(String::as_str),
                        )
                    })
                    .unwrap_or_default()
            }
        };
        Ok(files
            .into_iter()
            .map(|(id, name)| (id, format!("{DIR}/{name}")))
            .collect())
    }
}

/// The template files in a directory: a checkout, or the directory of
/// templates itself.
fn coolify_local_files(cfg: &CatalogConfig) -> Result<Files> {
    let root = Path::new(&cfg.location);
    let rel_dir = if root.join(DIR).is_dir() {
        DIR
    } else if root.join("compose").is_dir() {
        "compose"
    } else {
        ""
    };
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join(rel_dir))?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(id) = template_id(&name).filter(|_| e.path().is_file()) {
            let rel = if rel_dir.is_empty() {
                name
            } else {
                format!("{rel_dir}/{name}")
            };
            out.push((id, rel));
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::dokploy::Status;

    const HELLO: &str = "# documentation: https://hello.example\n# slogan: Says hello.\n# category: tools\n# tags: hello, demo\n# logo: svgs/hello.svg\n# port: 80\n\nservices:\n  hello:\n    image: traefik/whoami:v1.10\n    environment:\n      - SERVICE_URL_HELLO_80\n";
    const TWO: &str = "# slogan: Two.\nservices:\n  t:\n    image: t:1\n    restart: always\n";

    fn add(c: &Catalogs, name: &str, location: &str) {
        c.add(CatalogConfig {
            name: name.into(),
            format: Format::Coolify,
            location: location.into(),
        })
        .unwrap();
    }

    fn refs(c: &Catalogs, catalog: &str) -> Vec<String> {
        let (all, errors) = c.list();
        assert!(errors.is_empty(), "{errors:?}");
        all.into_iter()
            .filter(|s| s.catalog == catalog)
            .map(|s| s.reference)
            .collect()
    }

    #[test]
    fn locations_are_normalized() {
        let n = normalize_location;
        assert_eq!(
            n("https://github.com/coollabsio/coolify"),
            "https://raw.githubusercontent.com/coollabsio/coolify/HEAD"
        );
        assert_eq!(
            n("https://github.com/coollabsio/coolify/"),
            "https://raw.githubusercontent.com/coollabsio/coolify/HEAD"
        );
        assert_eq!(
            n("https://github.com/coollabsio/coolify/tree/v4.x"),
            "https://raw.githubusercontent.com/coollabsio/coolify/v4.x"
        );
        for same in [
            "https://raw.githubusercontent.com/coollabsio/coolify/main",
            "https://github.com/coollabsio/coolify/blob/main/x",
            "https://example.com/coolify",
            "/srv/coolify",
        ] {
            assert_eq!(n(same), same);
        }
        assert_eq!(
            github_listing("https://raw.githubusercontent.com/coollabsio/coolify/main").as_deref(),
            Some(
                "https://api.github.com/repos/coollabsio/coolify/contents/templates/compose?ref=main"
            )
        );
        assert_eq!(github_listing("https://example.com/x"), None);
    }

    #[test]
    fn a_checkout_is_listed_and_translated() {
        let dir = tempfile::tempdir().unwrap();
        let compose = dir.path().join("checkout/templates/compose");
        std::fs::create_dir_all(&compose).unwrap();
        std::fs::write(compose.join("hello.yaml"), HELLO).unwrap();
        std::fs::write(compose.join("other-app.yml"), TWO).unwrap();
        std::fs::write(
            compose.join("off.yaml"),
            "# ignore: true\n# slogan: Off.\nservices:\n  o:\n    image: o:1\n",
        )
        .unwrap();
        std::fs::write(compose.join("notes.txt"), "not a template").unwrap();
        std::fs::write(compose.join("bad name.yaml"), HELLO).unwrap();
        let c = Catalogs::new(dir.path());
        add(
            &c,
            "cool",
            &dir.path().join("checkout").display().to_string(),
        );
        assert_eq!(refs(&c, "cool"), ["cool/hello", "cool/other-app"]);
        let (all, _) = c.list();
        let hello = all.iter().find(|s| s.reference == "cool/hello").unwrap();
        assert_eq!(hello.format, Format::Coolify);
        assert_eq!(hello.name, "Hello");
        assert_eq!(hello.description, "Says hello.");
        assert_eq!(hello.tags, ["tools", "hello", "demo"]);
        assert_eq!(hello.links["docs"], "https://hello.example");
        assert_eq!(
            hello.logo, None,
            "a checkout has no URL to serve logos from"
        );
        let r = c.get("cool/hello").unwrap();
        let t = r.template.unwrap();
        assert_eq!(t.apps[0].image, "docker:traefik/whoami:v1.10");
        assert_eq!(t.apps[0].domains[0]["port"], 80);
        assert_eq!(r.report.unwrap().status, Status::Clean);
        assert_eq!(r.summary.format, Format::Coolify);
        assert!(
            c.get("cool/off").is_err(),
            "an ignored template is not offered"
        );
        assert!(c.get("cool/nope").is_err());
        assert!(
            c.get("hello").unwrap().template.is_some(),
            "a bare id finds it"
        );
    }

    #[test]
    fn a_directory_of_templates_is_read_too() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["a/compose", "b"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
            std::fs::write(dir.path().join(sub).join("hello.yaml"), HELLO).unwrap();
        }
        let c = Catalogs::new(dir.path());
        add(&c, "a", &dir.path().join("a").display().to_string());
        add(&c, "b", &dir.path().join("b").display().to_string());
        assert!(c.get("a/hello").unwrap().template.is_some());
        assert!(c.get("b/hello").unwrap().template.is_some());
    }

    /// A fake https server: GitHub's listing, the files, the index.
    fn server(listing: Option<&'static str>, index: bool, log: Arc<Mutex<Vec<String>>>) -> FetchFn {
        Arc::new(move |u: &str| {
            log.lock().unwrap().push(u.to_string());
            let raw = "https://raw.githubusercontent.com/coollabsio/coolify/main/";
            if u.starts_with("https://api.github.com/") {
                return listing
                    .map(|l| l.as_bytes().to_vec())
                    .ok_or_else(|| Error::invalid("HTTP 403"));
            }
            match u.strip_prefix(raw) {
                Some("templates/compose/hello.yaml") => Ok(HELLO.as_bytes().to_vec()),
                Some("templates/compose/two.yaml") => Ok(TWO.as_bytes().to_vec()),
                Some("templates/service-templates.json") if index => {
                    Ok(br#"{"hello": {}, "two": {}, "gone": {}}"#.to_vec())
                }
                _ => Err(Error::invalid("HTTP 404")),
            }
        })
    }

    #[test]
    fn a_remote_catalog_lists_github_and_reads_each_header() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let listing = r#"[{"name":"hello.yaml","type":"file"},{"name":"two.yaml","type":"file"},{"name":"README.md","type":"file"},{"name":"sub","type":"dir"},{"name":"gone.yaml","type":"file"}]"#;
        let c = Catalogs::with_fetch(dir.path(), server(Some(listing), true, log.clone()));
        // The GitHub page of a repository is the same place.
        add(
            &c,
            "cool",
            "https://github.com/coollabsio/coolify/tree/main",
        );
        // `gone` is listed but cannot be read: skipped.
        assert_eq!(refs(&c, "cool"), ["cool/hello", "cool/two"]);
        let (all, _) = c.list();
        let hello = all.iter().find(|s| s.reference == "cool/hello").unwrap();
        assert_eq!(
            hello.logo.as_deref(),
            Some("https://raw.githubusercontent.com/coollabsio/coolify/main/public/svgs/hello.svg")
        );
        let seen = log.lock().unwrap().clone();
        assert!(
            seen[0].starts_with(
                "https://api.github.com/repos/coollabsio/coolify/contents/templates/compose"
            ),
            "{seen:?}"
        );
        assert!(
            !seen.iter().any(|u| u.contains("service-templates.json")),
            "{seen:?}"
        );
        // The compose is read from the cache: no new fetch to translate.
        let n = seen.len();
        let r = c.get("cool/hello").unwrap();
        assert_eq!(r.report.unwrap().status, Status::Clean);
        assert_eq!(log.lock().unwrap().len(), n);
    }

    #[test]
    fn without_github_the_generated_index_lists_the_templates() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let c = Catalogs::with_fetch(dir.path(), server(None, true, log.clone()));
        add(
            &c,
            "cool",
            "https://raw.githubusercontent.com/coollabsio/coolify/main",
        );
        assert_eq!(refs(&c, "cool"), ["cool/hello", "cool/two"]);
        // A catalog that cannot be read at all is an error.
        let dir = tempfile::tempdir().unwrap();
        let c = Catalogs::with_fetch(dir.path(), server(None, false, log));
        add(
            &c,
            "cool",
            "https://raw.githubusercontent.com/coollabsio/coolify/main",
        );
        let (_, errors) = c.list();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("cool"), "{errors:?}");
    }
}
