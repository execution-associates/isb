//! OCI images on the wire: an OCI image layout in a tar (what BuildKit's
//! `type=oci` exporter writes) and the distribution API to push it, resolve
//! tags and delete manifests.
//!
//! The layout comes out of a build sandbox, so it is untrusted: names are
//! looked up only at the paths a digest dictates, sizes are bounded by the
//! file, manifests by [`MAX_MANIFEST`], and every manifest's digest is
//! checked here while the registry checks every blob's on upload.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

pub const MT_OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const MT_OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const MT_DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
pub const MT_DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// What a manifest request accepts: every kind isb pushes.
const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";

/// The largest manifest or index read into memory.
pub const MAX_MANIFEST: u64 = 4 << 20;
/// How deep an index may nest.
const MAX_DEPTH: usize = 3;

/// `sha256:` and 64 lowercase hex digits: the only digest isb handles, and
/// safe to put in a URL or a path.
pub fn valid_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The `sha256:` digest of some bytes.
pub fn digest_of(b: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, b);
    let mut s = String::from("sha256:");
    for x in d.as_ref() {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// A content descriptor.
#[derive(Debug, Clone, Deserialize)]
pub struct Descriptor {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub digest: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct Index {
    #[serde(default)]
    manifests: Vec<Descriptor>,
}

#[derive(Deserialize)]
struct Manifest {
    config: Descriptor,
    #[serde(default)]
    layers: Vec<Descriptor>,
}

fn is_index(mt: &str) -> bool {
    mt == MT_OCI_INDEX || mt == MT_DOCKER_LIST
}

fn is_manifest(mt: &str) -> bool {
    mt == MT_OCI_MANIFEST || mt == MT_DOCKER_MANIFEST
}

// ---------------------------------------------------------------------------
// The layout tar
// ---------------------------------------------------------------------------

/// An OCI image layout inside a tar file, indexed without unpacking it.
pub struct Layout {
    path: PathBuf,
    /// Regular files: normalized path -> (data offset, size).
    entries: HashMap<String, (u64, u64)>,
}

fn octal(field: &[u8]) -> Option<u64> {
    // GNU base-256 for sizes over 8 GiB.
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v: u64 = (field[0] & 0x7f) as u64;
        for b in &field[1..] {
            v = v.checked_mul(256)?.checked_add(*b as u64)?;
        }
        return Some(v);
    }
    let s: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn normalize(p: &str) -> String {
    p.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

/// `path=` from a pax extended header.
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    let mut rest: &str = &text;
    let mut found = None;
    while !rest.is_empty() {
        let (len, _) = rest.split_once(' ')?;
        let n: usize = len.parse().ok()?;
        if n == 0 || n > rest.len() {
            return found;
        }
        let rec = &rest[..n];
        if let Some((_, kv)) = rec.split_once(' ') {
            if let Some(v) = kv.strip_prefix("path=") {
                found = Some(v.trim_end_matches('\n').to_string());
            }
        }
        rest = &rest[n..];
    }
    found
}

impl Layout {
    /// Index the tar at `path`.
    pub fn open(path: &Path) -> Result<Layout> {
        let bad = |why: String| Error::invalid(format!("image archive {}: {why}", path.display()));
        let mut f = File::open(path)?;
        let len = f.metadata()?.len();
        let mut entries = HashMap::new();
        let mut pos: u64 = 0;
        let mut long_name: Option<String> = None;
        let mut hdr = [0u8; 512];
        loop {
            if pos + 512 > len {
                break;
            }
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut hdr)?;
            if hdr.iter().all(|b| *b == 0) {
                break;
            }
            let size = octal(&hdr[124..136]).ok_or_else(|| bad("bad size field".into()))?;
            let data = pos + 512;
            if data.checked_add(size).is_none_or(|end| end > len) {
                return Err(bad("an entry runs past the end".into()));
            }
            let kind = hdr[156];
            let mut name = cstr(&hdr[0..100]);
            if &hdr[257..262] == b"ustar" {
                let prefix = cstr(&hdr[345..500]);
                if !prefix.is_empty() {
                    name = format!("{prefix}/{name}");
                }
            }
            match kind {
                b'x' | b'L' => {
                    if size > 64 << 10 {
                        return Err(bad("an oversized extended header".into()));
                    }
                    let mut buf = vec![0u8; size as usize];
                    f.seek(SeekFrom::Start(data))?;
                    f.read_exact(&mut buf)?;
                    long_name = if kind == b'L' {
                        Some(cstr(&buf))
                    } else {
                        pax_path(&buf)
                    };
                }
                b'g' => {}
                b'0' | 0 => {
                    let n = long_name.take().unwrap_or(name);
                    entries.insert(normalize(&n), (data, size));
                }
                _ => {
                    long_name = None;
                }
            }
            pos = data + size.div_ceil(512) * 512;
        }
        Ok(Layout {
            path: path.to_path_buf(),
            entries,
        })
    }

    fn entry(&self, name: &str) -> Result<(u64, u64)> {
        self.entries.get(name).copied().ok_or_else(|| {
            Error::invalid(format!("image archive {}: no {name}", self.path.display()))
        })
    }

    fn blob_name(digest: &str) -> Result<String> {
        if !valid_digest(digest) {
            return Err(Error::invalid(format!(
                "image archive: unsupported digest {digest:?}"
            )));
        }
        Ok(format!("blobs/sha256/{}", &digest[7..]))
    }

    fn read_at(&self, (off, size): (u64, u64), limit: u64) -> Result<Vec<u8>> {
        if size > limit {
            return Err(Error::invalid(format!(
                "image archive {}: a {size}-byte manifest is over the {limit}-byte limit",
                self.path.display()
            )));
        }
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; size as usize];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// A blob read into memory (manifests and configs), checked against its
    /// digest.
    pub fn read_blob(&self, d: &Descriptor) -> Result<Vec<u8>> {
        let e = self.entry(&Self::blob_name(&d.digest)?)?;
        let b = self.read_at(e, MAX_MANIFEST)?;
        if digest_of(&b) != d.digest {
            return Err(Error::invalid(format!(
                "image archive: blob {} does not match its digest",
                d.digest
            )));
        }
        Ok(b)
    }

    /// A blob's bytes as a reader, and its size.
    pub fn blob_reader(&self, digest: &str) -> Result<(std::io::Take<File>, u64)> {
        let (off, size) = self.entry(&Self::blob_name(digest)?)?;
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(off))?;
        Ok((f.take(size), size))
    }

    /// The image the layout holds: `index.json`'s one manifest.
    pub fn top(&self) -> Result<Descriptor> {
        let raw = self.read_at(self.entry("index.json")?, MAX_MANIFEST)?;
        let idx: Index = serde_json::from_slice(&raw)
            .map_err(|e| Error::invalid(format!("image archive: index.json: {e}")))?;
        match idx.manifests.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(Error::invalid("image archive: index.json lists no image")),
            more => Err(Error::invalid(format!(
                "image archive: index.json lists {} images; a build exports one",
                more.len()
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// The registry API
// ---------------------------------------------------------------------------

/// A registry, reached at `base` (`https://127.0.0.1:5480`).
#[derive(Clone)]
pub struct Remote {
    agent: ureq::Agent,
    base: String,
}

/// Repositories are `<org>/<app>`, both validated by their owners; check
/// once more before a name goes into a URL.
fn check_repo(repo: &str) -> Result<()> {
    let ok = !repo.is_empty()
        && repo.len() <= 255
        && repo.split('/').all(|p| {
            !p.is_empty()
                && p.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        });
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!("invalid repository name {repo:?}")))
    }
}

/// A tag or a digest.
fn check_reference(r: &str) -> Result<()> {
    if valid_digest(r) || super::valid_tag(r) {
        Ok(())
    } else {
        Err(Error::invalid(format!("invalid tag or digest {r:?}")))
    }
}

impl Remote {
    /// `ca_pem`: the one CA to trust (none: the platform's roots, or plain
    /// http in tests).
    pub fn new(base: &str, ca_pem: Option<&str>, timeout: Duration) -> Result<Remote> {
        let mut cfg = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")));
        if let Some(pem) = ca_pem {
            let cert = ureq::tls::Certificate::from_pem(pem.as_bytes())
                .map_err(|e| Error::invalid(format!("registry CA: {e}")))?;
            cfg = cfg.tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::new_with_certs(&[cert]))
                    .build(),
            );
        }
        Ok(Remote {
            agent: cfg.build().into(),
            base: base.trim_end_matches('/').to_string(),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn err(&self, step: &str, e: impl std::fmt::Display) -> Error {
        Error::invalid(format!("registry {}: {step}: {e}", self.base))
    }

    fn status_err(&self, step: &str, mut r: ureq::http::Response<ureq::Body>) -> Error {
        let status = r.status().as_u16();
        let body = r
            .body_mut()
            .with_config()
            .limit(64 << 10)
            .read_to_string()
            .unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v["errors"].as_array().map(|a| {
                    a.iter()
                        .map(|e| {
                            format!(
                                "{} {}",
                                e["code"].as_str().unwrap_or(""),
                                e["message"].as_str().unwrap_or("")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
            })
            .unwrap_or(body);
        self.err(step, format!("HTTP {status} {}", detail.trim()))
    }

    /// Whether `repo` has the blob.
    fn blob_exists(&self, repo: &str, digest: &str) -> Result<bool> {
        let step = format!("check blob {digest}");
        let r = self
            .agent
            .head(format!("{}/v2/{repo}/blobs/{digest}", self.base))
            .call()
            .map_err(|e| self.err(&step, e))?;
        match r.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            _ => Err(self.status_err(&step, r)),
        }
    }

    /// Where an upload continues: the registry's `Location`, which must stay
    /// on this registry.
    fn location(&self, r: &ureq::http::Response<ureq::Body>, step: &str) -> Result<String> {
        let loc = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| self.err(step, "no Location"))?;
        if loc.starts_with('/') {
            Ok(format!("{}{loc}", self.base))
        } else if loc.starts_with(&format!("{}/", self.base)) {
            Ok(loc.to_string())
        } else {
            Err(self.err(step, format!("refusing to upload to {loc}")))
        }
    }

    /// Upload one blob, monolithically, unless the repository has it.
    /// Returns whether it was uploaded.
    pub fn put_blob(
        &self,
        repo: &str,
        digest: &str,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<bool> {
        check_repo(repo)?;
        if !valid_digest(digest) {
            return Err(self.err("upload", format!("unsupported digest {digest:?}")));
        }
        if self.blob_exists(repo, digest)? {
            return Ok(false);
        }
        let step = format!("start upload of {digest}");
        let r = self
            .agent
            .post(format!("{}/v2/{repo}/blobs/uploads/", self.base))
            .header("Content-Length", "0")
            .send_empty()
            .map_err(|e| self.err(&step, e))?;
        if r.status().as_u16() != 202 {
            return Err(self.status_err(&step, r));
        }
        let loc = self.location(&r, &step)?;
        let sep = if loc.contains('?') { '&' } else { '?' };
        let step = format!("upload {digest} ({size} bytes)");
        let r = self
            .agent
            .put(format!("{loc}{sep}digest={digest}"))
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", size.to_string())
            .send(ureq::SendBody::from_reader(body))
            .map_err(|e| self.err(&step, e))?;
        if r.status().as_u16() != 201 {
            return Err(self.status_err(&step, r));
        }
        Ok(true)
    }

    /// Put a manifest under `reference` (a tag or its digest). Returns the
    /// digest the registry gives it.
    pub fn put_manifest(
        &self,
        repo: &str,
        reference: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<String> {
        check_repo(repo)?;
        check_reference(reference)?;
        let step = format!("put manifest {repo}:{reference}");
        let r = self
            .agent
            .put(format!("{}/v2/{repo}/manifests/{reference}", self.base))
            .header("Content-Type", media_type)
            .send(bytes)
            .map_err(|e| self.err(&step, e))?;
        if r.status().as_u16() != 201 {
            return Err(self.status_err(&step, r));
        }
        let ours = digest_of(bytes);
        match r
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
        {
            Some(d) if d != ours => Err(self.err(
                &step,
                format!("the registry stored {d}, but the manifest is {ours}"),
            )),
            _ => Ok(ours),
        }
    }

    /// Push the layout's image as `repo:tag`. Returns its manifest digest.
    pub fn push(
        &self,
        layout: &Layout,
        repo: &str,
        tag: &str,
        log: &mut dyn FnMut(&str),
    ) -> Result<String> {
        check_repo(repo)?;
        check_reference(tag)?;
        let top = layout.top()?;
        let bytes = self.push_children(layout, repo, &top, 0, log)?;
        let digest = self.put_manifest(repo, tag, &top.media_type, &bytes)?;
        if digest != top.digest {
            return Err(self.err("push", "the top manifest does not match index.json"));
        }
        log(&format!("pushed {repo}:{tag} ({digest})"));
        Ok(digest)
    }

    /// Push everything `d` refers to; returns `d`'s own bytes.
    fn push_children(
        &self,
        layout: &Layout,
        repo: &str,
        d: &Descriptor,
        depth: usize,
        log: &mut dyn FnMut(&str),
    ) -> Result<Vec<u8>> {
        let bytes = layout.read_blob(d)?;
        // Some exporters leave the media type to the document itself.
        let mt = if d.media_type.is_empty() {
            serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v["mediaType"].as_str().map(String::from))
                .unwrap_or_default()
        } else {
            d.media_type.clone()
        };
        if is_index(&mt) {
            if depth >= MAX_DEPTH {
                return Err(self.err("push", "image indexes nest too deeply"));
            }
            let idx: Index = serde_json::from_slice(&bytes)
                .map_err(|e| self.err("push", format!("index {}: {e}", d.digest)))?;
            for m in &idx.manifests {
                let child = self.push_children(layout, repo, m, depth + 1, log)?;
                let cm = if m.media_type.is_empty() {
                    MT_OCI_MANIFEST
                } else {
                    &m.media_type
                };
                self.put_manifest(repo, &m.digest, cm, &child)?;
            }
        } else if is_manifest(&mt) {
            let m: Manifest = serde_json::from_slice(&bytes)
                .map_err(|e| self.err("push", format!("manifest {}: {e}", d.digest)))?;
            for b in std::iter::once(&m.config).chain(m.layers.iter()) {
                let (mut r, size) = layout.blob_reader(&b.digest)?;
                if size != b.size {
                    return Err(self.err(
                        "push",
                        format!("blob {} is {size} bytes, not {}", b.digest, b.size),
                    ));
                }
                if self.put_blob(repo, &b.digest, size, &mut r)? {
                    log(&format!("pushed blob {} ({size} bytes)", short(&b.digest)));
                }
            }
        } else {
            return Err(self.err("push", format!("unsupported media type {mt:?}")));
        }
        Ok(bytes)
    }

    /// The digest `reference` (a tag or a digest) names, or `None`.
    pub fn resolve(&self, repo: &str, reference: &str) -> Result<Option<String>> {
        check_repo(repo)?;
        check_reference(reference)?;
        let step = format!("resolve {repo}:{reference}");
        let r = self
            .agent
            .head(format!("{}/v2/{repo}/manifests/{reference}", self.base))
            .header("Accept", ACCEPT)
            .call()
            .map_err(|e| self.err(&step, e))?;
        match r.status().as_u16() {
            200 => r
                .headers()
                .get("docker-content-digest")
                .and_then(|v| v.to_str().ok())
                .filter(|d| valid_digest(d))
                .map(|d| Some(d.to_string()))
                .ok_or_else(|| self.err(&step, "no Docker-Content-Digest")),
            404 => Ok(None),
            _ => Err(self.status_err(&step, r)),
        }
    }

    /// A manifest's media type and bytes, checked against its digest.
    pub fn get_manifest(&self, repo: &str, digest: &str) -> Result<Option<(String, Vec<u8>)>> {
        check_repo(repo)?;
        if !valid_digest(digest) {
            return Err(self.err("get manifest", format!("not a digest: {digest:?}")));
        }
        let step = format!("get {repo}@{digest}");
        let mut r = self
            .agent
            .get(format!("{}/v2/{repo}/manifests/{digest}", self.base))
            .header("Accept", ACCEPT)
            .call()
            .map_err(|e| self.err(&step, e))?;
        match r.status().as_u16() {
            200 => {
                let mt = r
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(MT_OCI_MANIFEST)
                    .to_string();
                let b = r
                    .body_mut()
                    .with_config()
                    .limit(MAX_MANIFEST)
                    .read_to_vec()
                    .map_err(|e| self.err(&step, e))?;
                if digest_of(&b) != digest {
                    return Err(self.err(&step, "the manifest does not match its digest"));
                }
                Ok(Some((mt, b)))
            }
            404 => Ok(None),
            _ => Err(self.status_err(&step, r)),
        }
    }

    fn get_json(&self, url: &str, step: &str) -> Result<Option<Value>> {
        let mut r = self.agent.get(url).call().map_err(|e| self.err(step, e))?;
        match r.status().as_u16() {
            200 => {
                let b = r
                    .body_mut()
                    .with_config()
                    .limit(16 << 20)
                    .read_to_vec()
                    .map_err(|e| self.err(step, e))?;
                Ok(Some(
                    serde_json::from_slice(&b).map_err(|e| self.err(step, e))?,
                ))
            }
            404 => Ok(None),
            _ => Err(self.status_err(step, r)),
        }
    }

    /// A repository's tags (none when it does not exist).
    pub fn tags(&self, repo: &str) -> Result<Vec<String>> {
        check_repo(repo)?;
        let mut t = self.paged(
            &format!("/v2/{repo}/tags/list"),
            "tags",
            &format!("list tags of {repo}"),
        )?;
        t.sort();
        Ok(t)
    }

    /// Every repository.
    pub fn catalog(&self) -> Result<Vec<String>> {
        self.paged("/v2/_catalog", "repositories", "list repositories")
    }

    /// A paginated list (`n` and `last`, as the distribution API pages).
    fn paged(&self, path: &str, key: &str, step: &str) -> Result<Vec<String>> {
        const PAGE: usize = 1000;
        let mut out: Vec<String> = Vec::new();
        loop {
            let mut url = format!("{}{path}?n={PAGE}", self.base);
            if let Some(l) = out.last() {
                url.push_str(&format!("&last={}", crate::client::encode_query(l)));
            }
            let page: Vec<String> = self
                .get_json(&url, step)?
                .as_ref()
                .and_then(|v| v[key].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let n = page.len();
            out.extend(page);
            if n < PAGE || out.len() > 1_000_000 {
                return Ok(out);
            }
        }
    }

    /// Delete a manifest (and with it every tag pointing at it).
    pub fn delete_manifest(&self, repo: &str, digest: &str) -> Result<()> {
        check_repo(repo)?;
        if !valid_digest(digest) {
            return Err(self.err("delete", format!("not a digest: {digest:?}")));
        }
        let step = format!("delete {repo}@{digest}");
        let r = self
            .agent
            .delete(format!("{}/v2/{repo}/manifests/{digest}", self.base))
            .call()
            .map_err(|e| self.err(&step, e))?;
        match r.status().as_u16() {
            202 | 200 | 404 => Ok(()),
            _ => Err(self.status_err(&step, r)),
        }
    }
}

/// `sha256:0123456789ab` for logs.
pub fn short(d: &str) -> &str {
    &d[..d.len().min(19)]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// A tar file with these entries (ustar, short names).
    pub fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..108].copy_from_slice(b"0000644\0");
            h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            h[136..148].copy_from_slice(b"00000000000\0");
            h[156] = b'0';
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|b| *b as u32).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            out.extend_from_slice(&h);
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    /// An OCI layout tar with one image: (tar bytes, manifest digest).
    pub fn image_tar(layer: &[u8]) -> (Vec<u8>, String) {
        let config = br#"{"architecture":"amd64","os":"linux","config":{}}"#.to_vec();
        let (cd, ld) = (digest_of(&config), digest_of(layer));
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"{MT_OCI_MANIFEST}","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{cd}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"{ld}","size":{}}}]}}"#,
            config.len(),
            layer.len()
        );
        let md = digest_of(manifest.as_bytes());
        let index = format!(
            r#"{{"schemaVersion":2,"manifests":[{{"mediaType":"{MT_OCI_MANIFEST}","digest":"{md}","size":{}}}]}}"#,
            manifest.len()
        );
        let p = |d: &str| format!("blobs/sha256/{}", &d[7..]);
        let (pc, pl, pm) = (p(&cd), p(&ld), p(&md));
        let t = tar(&[
            ("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#),
            ("index.json", index.as_bytes()),
            (&pc, &config),
            (&pl, layer),
            (&pm, manifest.as_bytes()),
        ]);
        (t, md)
    }

    /// A registry that keeps blobs and manifests in memory and records
    /// every request, speaking plain HTTP on loopback.
    #[derive(Default)]
    pub struct Fake {
        pub blobs: BTreeMap<String, Vec<u8>>,
        /// (repo, reference) -> (media type, bytes)
        pub manifests: BTreeMap<(String, String), (String, Vec<u8>)>,
        pub log: Vec<String>,
    }

    pub fn fake() -> (String, Arc<Mutex<Fake>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let state = Arc::new(Mutex::new(Fake::default()));
        let s = state.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let s = s.clone();
                std::thread::spawn(move || serve(c, &s));
            }
        });
        (format!("http://{addr}"), state)
    }

    fn serve(c: TcpStream, s: &Mutex<Fake>) {
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut w = c;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let mut parts = line.split_whitespace();
            let (method, target) = (
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
            );
            let mut headers = BTreeMap::new();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
            }
            assert!(
                !headers.contains_key("transfer-encoding"),
                "uploads are length-delimited"
            );
            let len: usize = headers
                .get("content-length")
                .map(|v| v.parse().unwrap())
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let (path, query) = target.split_once('?').unwrap_or((&target, ""));
            let mut st = s.lock().unwrap();
            st.log.push(format!("{method} {path}"));
            let (status, extra, out): (u16, Vec<(String, String)>, Vec<u8>) =
                route(&mut st, &method, path, query, &headers, body);
            drop(st);
            let mut resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", out.len());
            for (k, v) in extra {
                resp.push_str(&format!("{k}: {v}\r\n"));
            }
            resp.push_str("\r\n");
            // A HEAD response has the headers of a GET and no body.
            let out = if method == "HEAD" { Vec::new() } else { out };
            if w.write_all(resp.as_bytes()).is_err() || w.write_all(&out).is_err() {
                return;
            }
        }
    }

    type Reply = (u16, Vec<(String, String)>, Vec<u8>);

    fn route(
        st: &mut Fake,
        method: &str,
        path: &str,
        query: &str,
        headers: &BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> Reply {
        let p = path.trim_start_matches("/v2/");
        let none = || {
            (
                404u16,
                vec![],
                b"{\"errors\":[{\"code\":\"NOT_FOUND\"}]}".to_vec(),
            )
        };
        if let Some((repo, rest)) = p.split_once("/blobs/uploads/") {
            return match method {
                "POST" => (
                    202,
                    vec![(
                        "Location".into(),
                        format!("/v2/{repo}/blobs/uploads/u1?_state=x"),
                    )],
                    vec![],
                ),
                "PUT" => {
                    assert_eq!(rest, "u1");
                    let d = query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("digest="))
                        .unwrap()
                        .replace("%3A", ":");
                    if digest_of(&body) != d {
                        return (
                            400,
                            vec![],
                            b"{\"errors\":[{\"code\":\"DIGEST_INVALID\"}]}".to_vec(),
                        );
                    }
                    st.blobs.insert(format!("{repo}@{d}"), body);
                    (201, vec![], vec![])
                }
                _ => none(),
            };
        }
        if let Some((repo, d)) = p.split_once("/blobs/") {
            return if st.blobs.contains_key(&format!("{repo}@{d}")) {
                (200, vec![], vec![])
            } else {
                none()
            };
        }
        if let Some((repo, reference)) = p.split_once("/manifests/") {
            let key = (repo.to_string(), reference.to_string());
            return match method {
                "PUT" => {
                    let d = digest_of(&body);
                    let mt = headers.get("content-type").cloned().unwrap_or_default();
                    st.manifests.insert(key, (mt.clone(), body.clone()));
                    st.manifests
                        .insert((repo.to_string(), d.clone()), (mt, body));
                    (201, vec![("Docker-Content-Digest".into(), d)], vec![])
                }
                "HEAD" | "GET" => match st.manifests.get(&key) {
                    Some((mt, b)) => (
                        200,
                        vec![
                            ("Docker-Content-Digest".into(), digest_of(b)),
                            ("Content-Type".into(), mt.clone()),
                        ],
                        b.clone(),
                    ),
                    None => none(),
                },
                "DELETE" => {
                    let before = st.manifests.len();
                    st.manifests
                        .retain(|(r, _), (_, b)| !(r == repo && digest_of(b) == reference));
                    if st.manifests.len() < before {
                        (202, vec![], vec![])
                    } else {
                        none()
                    }
                }
                _ => none(),
            };
        }
        if let Some(repo) = p.strip_suffix("/tags/list") {
            let mut tags: Vec<String> = st
                .manifests
                .keys()
                .filter(|(r, t)| r == repo && !t.starts_with("sha256:"))
                .map(|(_, t)| t.clone())
                .collect();
            tags.dedup();
            return (
                200,
                vec![],
                serde_json::to_vec(&serde_json::json!({"name": repo, "tags": tags})).unwrap(),
            );
        }
        if p == "_catalog" {
            let mut repos: Vec<String> = st.manifests.keys().map(|(r, _)| r.clone()).collect();
            repos.dedup();
            return (
                200,
                vec![],
                serde_json::to_vec(&serde_json::json!({"repositories": repos})).unwrap(),
            );
        }
        none()
    }

    fn layout_of(bytes: &[u8]) -> (tempfile::TempDir, Layout) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("image.tar");
        std::fs::write(&p, bytes).unwrap();
        let l = Layout::open(&p).unwrap();
        (d, l)
    }

    #[test]
    fn digests() {
        assert!(valid_digest(&digest_of(b"x")));
        assert!(!valid_digest("sha256:../../etc"));
        assert!(!valid_digest("sha512:00"));
        assert!(!valid_digest(&format!("sha256:{}", "A".repeat(64))));
    }

    #[test]
    fn layout_is_read_without_unpacking() {
        let (t, md) = image_tar(b"layer bytes");
        let (_d, l) = layout_of(&t);
        let top = l.top().unwrap();
        assert_eq!(top.digest, md);
        assert!(
            l.read_blob(&top)
                .unwrap()
                .starts_with(b"{\"schemaVersion\"")
        );
        let (mut r, n) = l.blob_reader(&digest_of(b"layer bytes")).unwrap();
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!((s.as_str(), n), ("layer bytes", 11));
        assert!(l.blob_reader(&digest_of(b"other")).is_err());
    }

    #[test]
    fn layout_rejects_lies() {
        // A blob whose content does not match the digest it is filed under.
        let fake = digest_of(b"claimed");
        let name = format!("blobs/sha256/{}", &fake[7..]);
        let t = tar(&[(&name, b"actual")]);
        let (_d, l) = layout_of(&t);
        let d = Descriptor {
            media_type: MT_OCI_MANIFEST.into(),
            digest: fake,
            size: 6,
        };
        assert!(l.read_blob(&d).is_err());
        // A size field past the end of the file.
        let mut t = tar(&[("index.json", b"{}")]);
        t[124..136].copy_from_slice(b"77777777777\0");
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("x.tar"), &t).unwrap();
        assert!(Layout::open(&d.path().join("x.tar")).is_err());
        // Two images in one export.
        let t = tar(&[(
            "index.json",
            br#"{"manifests":[{"digest":"sha256:00","size":1},{"digest":"sha256:01","size":1}]}"#,
        )]);
        let (_d, l) = layout_of(&t);
        assert!(l.top().is_err());
    }

    #[test]
    fn push_uploads_missing_blobs_then_the_manifest() {
        let (base, st) = fake();
        let r = Remote::new(&base, None, Duration::from_secs(10)).unwrap();
        let (t, md) = image_tar(b"some layer");
        let (_d, l) = layout_of(&t);
        let mut lines = Vec::new();
        let d = r
            .push(&l, "acme/web", "v1", &mut |s| lines.push(s.to_string()))
            .unwrap();
        assert_eq!(d, md);
        assert_eq!(
            r.resolve("acme/web", "v1").unwrap().as_deref(),
            Some(md.as_str())
        );
        assert_eq!(r.resolve("acme/web", "v2").unwrap(), None);
        assert_eq!(r.tags("acme/web").unwrap(), vec!["v1".to_string()]);
        let log = st.lock().unwrap().log.clone();
        let uploads = log
            .iter()
            .filter(|l| l.starts_with("PUT /v2/acme/web/blobs/uploads"))
            .count();
        assert_eq!(uploads, 2, "config and layer: {log:?}");
        let put = log
            .iter()
            .position(|l| l == "PUT /v2/acme/web/manifests/v1")
            .unwrap();
        let last_upload = log
            .iter()
            .rposition(|l| l.contains("/blobs/uploads/"))
            .unwrap();
        assert!(put > last_upload, "the manifest goes last: {log:?}");

        // A second push of the same image uploads nothing new.
        st.lock().unwrap().log.clear();
        r.push(&l, "acme/web", "v2", &mut |_| {}).unwrap();
        let log = st.lock().unwrap().log.clone();
        assert!(!log.iter().any(|l| l.contains("uploads")), "{log:?}");
        // Blobs are per repository: another one uploads its own.
        r.push(&l, "other/web", "v1", &mut |_| {}).unwrap();
        assert!(
            st.lock()
                .unwrap()
                .blobs
                .contains_key(&format!("other/web@{}", digest_of(b"some layer")))
        );

        let (mt, b) = r.get_manifest("acme/web", &md).unwrap().unwrap();
        assert_eq!((mt.as_str(), digest_of(&b)), (MT_OCI_MANIFEST, md.clone()));
        r.delete_manifest("acme/web", &md).unwrap();
        assert!(r.get_manifest("acme/web", &md).unwrap().is_none());
        assert_eq!(r.resolve("acme/web", "v1").unwrap(), None);
        assert!(r.resolve("acme/../x", "v1").is_err());
        assert!(r.resolve("acme/web", "bad tag").is_err());
    }
}
