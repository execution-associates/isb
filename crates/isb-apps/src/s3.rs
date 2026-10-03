//! A small S3 client for backups: AWS Signature Version 4 over ring and
//! ureq, no SDK.
//!
//! Path-style (`https://endpoint/bucket/key`, what MinIO and most
//! self-hosted stores want) or virtual-hosted (`https://bucket.endpoint/key`)
//! addressing. Uploads stream: [`Upload`] buffers one part at a time
//! ([`PART_SIZE`]), sends a single `PUT` when the whole object fit in one
//! part, and a multipart upload otherwise, aborted if anything fails.
//! Downloads are a reader over the response body.

use std::io::{Read, Write};
use std::time::Duration;

use ring::{digest, hmac};

use crate::error::{Error, Result};

/// Bytes buffered per part of a multipart upload. S3 wants at least 5 MiB
/// per part (but the last) and at most 10,000 parts: up to ~156 GiB.
pub const PART_SIZE: usize = 16 << 20;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Credentials for signing.
#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key", &self.access_key)
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

// --- Signature Version 4 ---------------------------------------------------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, b).as_ref())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&k, data).as_ref().to_vec()
}

/// The signing key for one day, region and service.
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac_sha256(&k, region.as_bytes());
    let k = hmac_sha256(&k, service.as_bytes());
    hmac_sha256(&k, b"aws4_request")
}

/// URI-encode per SigV4: unreserved characters stay, everything else is
/// `%XX` (uppercase); `/` stays only when `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What a request signs: method, the encoded path, the query pairs (not
/// encoded), headers (lowercase names) and the payload hash.
pub struct ToSign<'a> {
    pub method: &'a str,
    /// Already URI-encoded (`/bucket/my%20key`).
    pub path: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a [(String, String)],
    pub payload_sha256: &'a str,
}

/// The canonical request, the string to sign and the `Authorization` value.
pub struct Signed {
    pub canonical_request: String,
    pub string_to_sign: String,
    pub authorization: String,
}

/// Sign a request. `amz_date` is `YYYYMMDDTHHMMSSZ`; the request must carry
/// it (as `x-amz-date`) among `headers`.
pub fn sign(
    r: &ToSign,
    creds: &Credentials,
    region: &str,
    service: &str,
    amz_date: &str,
) -> Signed {
    let mut q: Vec<(String, String)> = r
        .query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    q.sort();
    let query = q
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut h: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(k, v)| {
            // Trim, and fold runs of spaces as SigV4 asks.
            let v = v.split_whitespace().collect::<Vec<_>>().join(" ");
            (k.to_ascii_lowercase(), v)
        })
        .collect();
    h.sort();
    let canonical_headers: String = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = h
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{query}\n{canonical_headers}\n{signed_headers}\n{}",
        r.method, r.path, r.payload_sha256
    );
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(&creds.secret_key, date, region, service);
    let signature = hex(&hmac_sha256(&key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key
    );
    Signed {
        canonical_request,
        string_to_sign,
        authorization,
    }
}

fn amz_now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    crate::cron::compact_utc(t)
}

// --- the client ------------------------------------------------------------

/// Where objects go.
#[derive(Debug, Clone)]
pub struct Bucket {
    /// `https://s3.eu-central-1.amazonaws.com`, `http://127.0.0.1:9000`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    /// `https://endpoint/bucket/key` rather than `https://bucket.endpoint/key`.
    pub path_style: bool,
    pub creds: Credentials,
}

/// One listed object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub last_modified: String,
}

#[derive(Clone)]
pub struct Client {
    b: Bucket,
    scheme: String,
    /// `host[:port]` as sent in the Host header.
    host: String,
    /// The path prefix before keys: `/bucket` (path style) or ``.
    base_path: String,
    agent: ureq::Agent,
}

/// The endpoint's scheme and host, refusing anything but a bare origin.
fn parse_endpoint(e: &str) -> Result<(String, String)> {
    let bad = |why: &str| Error::invalid(format!("endpoint {e:?}: {why}"));
    let (scheme, rest) = e
        .split_once("://")
        .ok_or_else(|| bad("give a URL, https://host[:port]"))?;
    if scheme != "https" && scheme != "http" {
        return Err(bad("http or https only"));
    }
    let host = rest.trim_end_matches('/');
    if host.is_empty() || host.contains(['/', '?', '#', '@', ' ']) {
        return Err(bad("an origin only (scheme, host and port; no path)"));
    }
    // The default port is not part of the Host header ureq sends.
    let host = match (scheme, host.rsplit_once(':')) {
        ("https", Some((h, "443"))) | ("http", Some((h, "80"))) => h.to_string(),
        _ => host.to_string(),
    };
    Ok((scheme.to_string(), host))
}

/// An S3 error reply's `Code` and `Message`.
fn s3_error(body: &str) -> String {
    let code = xml_text(body, "Code").unwrap_or_default();
    let msg = xml_text(body, "Message").unwrap_or_default();
    match (code.is_empty(), msg.is_empty()) {
        (true, true) => body.chars().take(300).collect(),
        _ => format!("{code}: {msg}"),
    }
}

/// The text of the first `<tag>...</tag>` in `xml`, unescaped.
pub fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let s = xml.find(&open)? + open.len();
    let e = xml[s..].find(&close)? + s;
    Some(xml_unescape(&xml[s..e]))
}

/// Each `<tag>...</tag>` block's inner text, in order.
pub fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(&after[..j]);
        rest = &after[j + close.len()..];
    }
    out
}

pub fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The body of a CompleteMultipartUpload request.
pub fn complete_body(parts: &[(u32, String)]) -> String {
    let mut s = String::from("<CompleteMultipartUpload>");
    for (n, etag) in parts {
        s.push_str(&format!(
            "<Part><PartNumber>{n}</PartNumber><ETag>{}</ETag></Part>",
            xml_escape(etag)
        ));
    }
    s.push_str("</CompleteMultipartUpload>");
    s
}

impl Client {
    pub fn new(b: Bucket) -> Result<Client> {
        let (scheme, host) = parse_endpoint(&b.endpoint)?;
        validate_bucket(&b.bucket)?;
        if b.region.is_empty() {
            return Err(Error::invalid("region: required (us-east-1 for MinIO)"));
        }
        let (host, base_path) = if b.path_style {
            (host, format!("/{}", uri_encode(&b.bucket, false)))
        } else {
            (format!("{}.{host}", b.bucket), String::new())
        };
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(300)))
            .timeout_send_body(Some(Duration::from_secs(1800)))
            // A restore reads a whole dump through one response.
            .timeout_recv_body(Some(Duration::from_secs(12 * 3600)))
            .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Client {
            b,
            scheme,
            host,
            base_path,
            agent,
        })
    }

    pub fn bucket(&self) -> &Bucket {
        &self.b
    }

    fn path(&self, key: &str) -> String {
        if key.is_empty() {
            return if self.base_path.is_empty() {
                "/".into()
            } else {
                self.base_path.clone()
            };
        }
        format!("{}/{}", self.base_path, uri_encode(key, true))
    }

    /// Send a signed request; `(status, response)`.
    fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(String, String)],
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let path = self.path(key);
        let date = amz_now();
        let payload = if body.is_empty() {
            EMPTY_SHA256.to_string()
        } else {
            sha256_hex(body)
        };
        let mut headers = vec![
            ("host".to_string(), self.host.clone()),
            ("x-amz-content-sha256".to_string(), payload.clone()),
            ("x-amz-date".to_string(), date.clone()),
        ];
        for (k, v) in extra {
            headers.push((k.to_string(), v.to_string()));
        }
        let signed = sign(
            &ToSign {
                method,
                path: &path,
                query,
                headers: &headers,
                payload_sha256: &payload,
            },
            &self.b.creds,
            &self.b.region,
            "s3",
            &date,
        );
        let mut url = format!("{}://{}{path}", self.scheme, self.host);
        if !query.is_empty() {
            url.push('?');
            url.push_str(
                &query
                    .iter()
                    .map(|(k, v)| {
                        if v.is_empty() && k == "uploads" {
                            k.clone()
                        } else {
                            format!("{}={}", uri_encode(k, false), uri_encode(v, false))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }
        let mut req = ureq::http::Request::builder().method(method).uri(&url);
        for (k, v) in &headers {
            if k != "host" {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req = req.header("authorization", signed.authorization.as_str());
        let built = |e: ureq::http::Error| Error::invalid(format!("s3 {method} {key}: {e}"));
        let r = if body.is_empty() && matches!(method, "GET" | "HEAD" | "DELETE") {
            self.agent.run(req.body(()).map_err(built)?)
        } else {
            self.agent.run(req.body(body).map_err(built)?)
        };
        r.map_err(|e| Error::invalid(format!("s3 {method} {}: {e}", self.describe(key))))
    }

    fn describe(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.b.bucket)
    }

    fn fail(&self, step: &str, key: &str, mut r: ureq::http::Response<ureq::Body>) -> Error {
        let status = r.status().as_u16();
        let body = r
            .body_mut()
            .with_config()
            .limit(64 << 10)
            .read_to_string()
            .unwrap_or_default();
        Error::invalid(format!(
            "s3 {step} {}: HTTP {status} {}",
            self.describe(key),
            s3_error(&body)
        ))
    }

    /// Upload one object in a single request.
    pub fn put(&self, key: &str, body: &[u8]) -> Result<()> {
        let r = self.send("PUT", key, &[], &[], body)?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(self.fail("put", key, r))
        }
    }

    /// Create the bucket; one that exists and is ours is fine.
    pub fn create_bucket(&self) -> Result<()> {
        let mut r = self.send("PUT", "", &[], &[], &[])?;
        if r.status().is_success() {
            return Ok(());
        }
        let body = r
            .body_mut()
            .with_config()
            .limit(64 << 10)
            .read_to_string()
            .unwrap_or_default();
        if body.contains("BucketAlreadyOwnedByYou") {
            return Ok(());
        }
        Err(Error::invalid(format!(
            "s3 create bucket {}: HTTP {} {}",
            self.b.bucket,
            r.status().as_u16(),
            s3_error(&body)
        )))
    }

    /// The object's size, or `None` when it does not exist.
    pub fn head(&self, key: &str) -> Result<Option<u64>> {
        let r = self.send("HEAD", key, &[], &[], &[])?;
        match r.status().as_u16() {
            200 => Ok(Some(
                r.headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            )),
            404 => Ok(None),
            s => Err(Error::invalid(format!(
                "s3 head {}: HTTP {s}",
                self.describe(key)
            ))),
        }
    }

    /// A reader over the object's bytes.
    pub fn get(&self, key: &str) -> Result<(u64, Box<dyn Read + Send>)> {
        let r = self.send("GET", key, &[], &[], &[])?;
        if !r.status().is_success() {
            return Err(self.fail("get", key, r));
        }
        let len = r
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Ok((len, Box::new(r.into_body().into_reader())))
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        let r = self.send("DELETE", key, &[], &[], &[])?;
        if r.status().is_success() || r.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(self.fail("delete", key, r))
        }
    }

    /// Every object under `prefix` (ListObjectsV2, following continuation).
    pub fn list(&self, prefix: &str) -> Result<Vec<Object>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..1000 {
            let mut q = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
            ];
            if let Some(t) = &token {
                q.push(("continuation-token".to_string(), t.clone()));
            }
            let mut r = self.send("GET", "", &q, &[], &[])?;
            if !r.status().is_success() {
                return Err(self.fail("list", prefix, r));
            }
            let body = r
                .body_mut()
                .with_config()
                .limit(32 << 20)
                .read_to_string()
                .map_err(|e| Error::invalid(format!("s3 list {prefix}: {e}")))?;
            out.extend(parse_list(&body));
            match (
                xml_text(&body, "IsTruncated").as_deref(),
                xml_text(&body, "NextContinuationToken"),
            ) {
                (Some("true"), Some(t)) => token = Some(t),
                _ => return Ok(out),
            }
        }
        Err(Error::invalid(format!(
            "s3 list {prefix}: more than 1000 pages"
        )))
    }

    fn create_multipart(&self, key: &str, content_type: &str) -> Result<String> {
        let q = [("uploads".to_string(), String::new())];
        let mut r = self.send("POST", key, &q, &[("content-type", content_type)], &[])?;
        if !r.status().is_success() {
            return Err(self.fail("create multipart upload", key, r));
        }
        let body = r
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_string()
            .unwrap_or_default();
        xml_text(&body, "UploadId").ok_or_else(|| {
            Error::invalid(format!(
                "s3 create multipart upload {}: no UploadId in the reply",
                self.describe(key)
            ))
        })
    }

    fn upload_part(&self, key: &str, upload: &str, n: u32, body: &[u8]) -> Result<String> {
        let q = [
            ("partNumber".to_string(), n.to_string()),
            ("uploadId".to_string(), upload.to_string()),
        ];
        let r = self.send("PUT", key, &q, &[], body)?;
        if !r.status().is_success() {
            return Err(self.fail(&format!("upload part {n}"), key, r));
        }
        r.headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "s3 upload part {n} {}: no ETag",
                    self.describe(key)
                ))
            })
    }

    fn complete_multipart(&self, key: &str, upload: &str, parts: &[(u32, String)]) -> Result<()> {
        let q = [("uploadId".to_string(), upload.to_string())];
        let body = complete_body(parts);
        let mut r = self.send(
            "POST",
            key,
            &q,
            &[("content-type", "application/xml")],
            body.as_bytes(),
        )?;
        if !r.status().is_success() {
            return Err(self.fail("complete multipart upload", key, r));
        }
        // S3 can answer 200 with an error document.
        let text = r
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_string()
            .unwrap_or_default();
        if text.contains("<Error>") {
            return Err(Error::invalid(format!(
                "s3 complete multipart upload {}: {}",
                self.describe(key),
                s3_error(&text)
            )));
        }
        Ok(())
    }

    fn abort_multipart(&self, key: &str, upload: &str) {
        let q = [("uploadId".to_string(), upload.to_string())];
        let _ = self.send("DELETE", key, &q, &[], &[]);
    }

    /// Start a streaming upload of `key`.
    pub fn upload(&self, key: &str, content_type: &str) -> Upload {
        Upload {
            c: self.clone(),
            key: key.to_string(),
            content_type: content_type.to_string(),
            buf: Vec::new(),
            upload_id: None,
            parts: Vec::new(),
            total: 0,
            part_size: PART_SIZE,
            failed: None,
        }
    }
}

fn parse_list(body: &str) -> Vec<Object> {
    xml_blocks(body, "Contents")
        .into_iter()
        .filter_map(|c| {
            Some(Object {
                key: xml_text(c, "Key")?,
                size: xml_text(c, "Size")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                last_modified: xml_text(c, "LastModified").unwrap_or_default(),
            })
        })
        .collect()
}

/// A bucket name S3 accepts (3-63 of `[a-z0-9.-]`).
pub fn validate_bucket(b: &str) -> Result<()> {
    let ok = (3..=63).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
        && b.starts_with(|c: char| c.is_ascii_alphanumeric())
        && b.ends_with(|c: char| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "bucket {b:?}: 3-63 characters of [a-z0-9.-]"
        )))
    }
}

/// A streaming upload: write to it, then [`Upload::finish`]. Holds at most
/// one part in memory. Dropped unfinished, a multipart upload is aborted.
pub struct Upload {
    c: Client,
    key: String,
    content_type: String,
    buf: Vec<u8>,
    upload_id: Option<String>,
    parts: Vec<(u32, String)>,
    total: u64,
    part_size: usize,
    failed: Option<String>,
}

impl Upload {
    /// Use smaller parts (tests; S3 itself wants at least 5 MiB).
    pub fn part_size(mut self, n: usize) -> Self {
        self.part_size = n.max(1);
        self
    }

    fn flush_part(&mut self) -> Result<()> {
        if self.upload_id.is_none() {
            self.upload_id = Some(self.c.create_multipart(&self.key, &self.content_type)?);
        }
        let id = self.upload_id.clone().unwrap_or_default();
        let n = self.parts.len() as u32 + 1;
        if n > 10_000 {
            return Err(Error::invalid("s3: more than 10,000 parts"));
        }
        let etag = self.c.upload_part(&self.key, &id, n, &self.buf)?;
        self.parts.push((n, etag));
        self.buf.clear();
        Ok(())
    }

    /// Send what is left and complete the object. Returns its size.
    pub fn finish(mut self) -> Result<u64> {
        if let Some(e) = self.failed.take() {
            return Err(Error::invalid(e));
        }
        match self.upload_id.clone() {
            None => {
                let body = std::mem::take(&mut self.buf);
                self.c.put(&self.key, &body)?;
            }
            Some(id) => {
                if !self.buf.is_empty() {
                    self.flush_part()?;
                }
                let parts = std::mem::take(&mut self.parts);
                self.c.complete_multipart(&self.key, &id, &parts)?;
                self.upload_id = None;
            }
        }
        Ok(self.total)
    }
}

impl Write for Upload {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if let Some(e) = &self.failed {
            return Err(std::io::Error::other(e.clone()));
        }
        let room = self.part_size - self.buf.len();
        let n = data.len().min(room);
        self.buf.extend_from_slice(&data[..n]);
        self.total += n as u64;
        if self.buf.len() >= self.part_size {
            if let Err(e) = self.flush_part() {
                self.failed = Some(e.to_string());
                return Err(std::io::Error::other(e.to_string()));
            }
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if let Some(id) = self.upload_id.take() {
            self.c.abort_multipart(&self.key, &id);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::BufRead;
    use std::sync::{Arc, Mutex};

    fn creds(a: &str, s: &str) -> Credentials {
        Credentials {
            access_key: a.into(),
            secret_key: s.into(),
        }
    }

    /// AWS's published example of the derived signing key (Signature
    /// Version 4 documentation, "Deriving the signing key").
    #[test]
    fn signing_key_vector() {
        let k = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20120215",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex(&k),
            "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
        );
    }

    /// The `get-vanilla` case of AWS's SigV4 test suite.
    #[test]
    fn suite_get_vanilla() {
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let s = sign(
            &ToSign {
                method: "GET",
                path: "/",
                query: &[],
                headers: &headers,
                payload_sha256: EMPTY_SHA256,
            },
            &creds("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"),
            "us-east-1",
            "service",
            "20150830T123600Z",
        );
        assert_eq!(
            s.canonical_request,
            "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            s.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    const S3_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn s3_sign(
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        payload: &str,
    ) -> String {
        let q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let h: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let s = sign(
            &ToSign {
                method,
                path,
                query: &q,
                headers: &h,
                payload_sha256: payload,
            },
            &creds("AKIAIOSFODNN7EXAMPLE", S3_SECRET),
            "us-east-1",
            "s3",
            "20130524T000000Z",
        );
        s.authorization
            .rsplit_once("Signature=")
            .unwrap()
            .1
            .to_string()
    }

    /// The S3 examples from AWS's "Signature Calculations for the
    /// Authorization Header" (examplebucket, 2013-05-24).
    #[test]
    fn s3_documented_examples() {
        // GET Object with a Range header.
        assert_eq!(
            s3_sign(
                "GET",
                "/test.txt",
                &[],
                &[
                    ("Host", "examplebucket.s3.amazonaws.com"),
                    ("Range", "bytes=0-9"),
                    ("x-amz-content-sha256", EMPTY_SHA256),
                    ("x-amz-date", "20130524T000000Z"),
                ],
                EMPTY_SHA256,
            ),
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        // PUT Object.
        let body = sha256_hex(b"Welcome to Amazon S3.");
        assert_eq!(
            body,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        assert_eq!(
            s3_sign(
                "PUT",
                "/test%24file.text",
                &[],
                &[
                    ("Host", "examplebucket.s3.amazonaws.com"),
                    ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
                    ("x-amz-date", "20130524T000000Z"),
                    ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
                    ("x-amz-content-sha256", &body),
                ],
                &body,
            ),
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
        // GET Bucket lifecycle: a query parameter with no value.
        assert_eq!(
            s3_sign(
                "GET",
                "/",
                &[("lifecycle", "")],
                &[
                    ("Host", "examplebucket.s3.amazonaws.com"),
                    ("x-amz-date", "20130524T000000Z"),
                    ("x-amz-content-sha256", EMPTY_SHA256),
                ],
                EMPTY_SHA256,
            ),
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
        // GET Bucket (list objects): sorted query parameters.
        assert_eq!(
            s3_sign(
                "GET",
                "/",
                &[("prefix", "J"), ("max-keys", "2")],
                &[
                    ("Host", "examplebucket.s3.amazonaws.com"),
                    ("x-amz-date", "20130524T000000Z"),
                    ("x-amz-content-sha256", EMPTY_SHA256),
                ],
                EMPTY_SHA256,
            ),
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn encoding_and_xml() {
        assert_eq!(uri_encode("a b/c~d$", true), "a%20b/c~d%24");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(uri_encode("é", false), "%C3%A9");
        let x = "<R><Contents><Key>a&amp;b</Key><Size>3</Size><LastModified>t</LastModified></Contents>\
                 <Contents><Key>c</Key><Size>10</Size></Contents><IsTruncated>false</IsTruncated></R>";
        let l = parse_list(x);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].key, "a&b");
        assert_eq!(l[1].size, 10);
        assert_eq!(
            s3_error("<Error><Code>NoSuchBucket</Code><Message>gone</Message></Error>"),
            "NoSuchBucket: gone"
        );
        assert_eq!(
            complete_body(&[(1, "\"e1\"".into()), (2, "\"e2\"".into())]),
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>&quot;e1&quot;</ETag></Part>\
             <Part><PartNumber>2</PartNumber><ETag>&quot;e2&quot;</ETag></Part></CompleteMultipartUpload>"
        );
        assert!(parse_endpoint("ftp://x").is_err());
        assert!(parse_endpoint("https://x/path").is_err());
        assert_eq!(
            parse_endpoint("https://s3.example.com:443").unwrap().1,
            "s3.example.com"
        );
        assert_eq!(
            parse_endpoint("http://127.0.0.1:9000/").unwrap().1,
            "127.0.0.1:9000"
        );
        assert!(validate_bucket("ab").is_err());
        assert!(validate_bucket("My_Bucket").is_err());
        validate_bucket("isb-backups.eu").unwrap();
    }

    /// One request as the fake S3 saw it.
    #[derive(Debug, Clone)]
    pub(crate) struct Seen {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    /// A loopback S3 that records requests, stores objects, and answers the
    /// multipart calls. Checks every request's signature with the secret.
    #[expect(
        clippy::too_many_lines,
        clippy::excessive_nesting,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    pub(crate) fn fake_s3(secret: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let s2 = seen.clone();
        type Store = std::collections::BTreeMap<String, Vec<u8>>;
        type Parts = std::collections::BTreeMap<u32, Vec<u8>>;
        let state: Arc<Mutex<(Store, Parts)>> = Default::default();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut conn) = conn else { continue };
                let (s2, state) = (s2.clone(), state.clone());
                // One thread per connection: pooled connections stay open.
                std::thread::spawn(move || {
                    let mut r = std::io::BufReader::new(conn.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        let mut it = line.split_whitespace();
                        let method = it.next().unwrap_or("").to_string();
                        let target = it.next().unwrap_or("").to_string();
                        let mut headers = Vec::new();
                        loop {
                            let mut h = String::new();
                            r.read_line(&mut h).unwrap();
                            let h = h.trim_end();
                            if h.is_empty() {
                                break;
                            }
                            let (k, v) = h.split_once(':').unwrap();
                            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                        }
                        let get =
                            |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
                        let len: usize = get("content-length")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let mut body = vec![0; len];
                        r.read_exact(&mut body).unwrap();
                        // Verify the signature as S3 would.
                        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                        let q: Vec<(String, String)> = query
                            .split('&')
                            .filter(|s| !s.is_empty())
                            .map(|p| {
                                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                                (pct_decode(k), pct_decode(v))
                            })
                            .collect();
                        let auth = get("authorization").unwrap_or_default();
                        let signed: Vec<&str> = auth
                            .split("SignedHeaders=")
                            .nth(1)
                            .unwrap_or("")
                            .split(',')
                            .next()
                            .unwrap()
                            .split(';')
                            .collect();
                        let hs: Vec<(String, String)> = signed
                            .iter()
                            .map(|k| (k.to_string(), get(k).unwrap_or_default()))
                            .collect();
                        let date = get("x-amz-date").unwrap_or_default();
                        let payload = get("x-amz-content-sha256").unwrap_or_default();
                        let ok_sig = payload == sha256_hex(&body)
                            && sign(
                                &ToSign {
                                    method: &method,
                                    path,
                                    query: &q,
                                    headers: &hs,
                                    payload_sha256: &payload,
                                },
                                &Credentials {
                                    access_key: "AK".into(),
                                    secret_key: secret.into(),
                                },
                                "us-east-1",
                                "s3",
                                &date,
                            )
                            .authorization
                                == auth;
                        s2.lock().unwrap().push(Seen {
                            method: method.clone(),
                            target: target.clone(),
                            headers: headers.clone(),
                            body: body.clone(),
                        });
                        let key = pct_decode(path.splitn(3, '/').nth(2).unwrap_or(""));
                        let mut st = state.lock().unwrap();
                        let (objects, parts) = &mut *st;
                        let has = |k: &str| q.iter().any(|(n, _)| n == k);
                        let qv = |k: &str| {
                            q.iter()
                                .find(|(n, _)| n == k)
                                .map(|(_, v)| v.clone())
                                .unwrap_or_default()
                        };
                        let (status, extra, resp): (u16, String, Vec<u8>) = if !ok_sig {
                            (
                            403,
                            String::new(),
                            b"<Error><Code>SignatureDoesNotMatch</Code><Message>no</Message></Error>"
                                .to_vec(),
                        )
                        } else {
                            match method.as_str() {
                                "POST" if has("uploads") => {
                                    parts.clear();
                                    (200, String::new(), b"<InitiateMultipartUploadResult><UploadId>up-1</UploadId></InitiateMultipartUploadResult>".to_vec())
                                }
                                "PUT" if has("partNumber") => {
                                    let n: u32 = qv("partNumber").parse().unwrap();
                                    parts.insert(n, body.clone());
                                    (200, format!("ETag: \"etag-{n}\"\r\n"), vec![])
                                }
                                "POST" if has("uploadId") => {
                                    let all: Vec<u8> = parts.values().flatten().copied().collect();
                                    objects.insert(key.clone(), all);
                                    (
                                        200,
                                        String::new(),
                                        b"<CompleteMultipartUploadResult/>".to_vec(),
                                    )
                                }
                                "DELETE" if has("uploadId") => (204, String::new(), vec![]),
                                "PUT" => {
                                    objects.insert(key.clone(), body.clone());
                                    (200, String::new(), vec![])
                                }
                                "HEAD" => match objects.get(&key) {
                                    Some(o) => {
                                        (200, format!("Content-Length: {}\r\n", o.len()), vec![])
                                    }
                                    None => (404, String::new(), vec![]),
                                },
                                "GET" if key.is_empty() => {
                                    let mut x = String::from("<ListBucketResult>");
                                    for (k, v) in objects.iter() {
                                        if k.starts_with(&qv("prefix")) {
                                            x.push_str(&format!(
                                            "<Contents><Key>{k}</Key><Size>{}</Size></Contents>",
                                            v.len()
                                        ));
                                        }
                                    }
                                    x.push_str(
                                        "<IsTruncated>false</IsTruncated></ListBucketResult>",
                                    );
                                    (200, String::new(), x.into_bytes())
                                }
                                "GET" => match objects.get(&key) {
                                    Some(o) => (200, String::new(), o.clone()),
                                    None => (404, String::new(), vec![]),
                                },
                                "DELETE" => {
                                    objects.remove(&key);
                                    (204, String::new(), vec![])
                                }
                                _ => (400, String::new(), vec![]),
                            }
                        };
                        // HEAD carries the object's length but no body.
                        let len_header = if method == "HEAD" {
                            String::new()
                        } else {
                            format!("Content-Length: {}\r\n", resp.len())
                        };
                        let head = format!("HTTP/1.1 {status} X\r\n{extra}{len_header}\r\n");
                        if conn.write_all(head.as_bytes()).is_err() {
                            break;
                        }
                        if method != "HEAD" {
                            let _ = conn.write_all(&resp);
                        }
                    }
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn pct_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    pub(crate) fn client(endpoint: &str, secret: &str) -> Client {
        Client::new(Bucket {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: "backups".into(),
            path_style: true,
            creds: creds("AK", secret),
        })
        .unwrap()
    }

    #[test]
    fn multipart_against_a_fake_s3() {
        let (ep, seen) = fake_s3("sk");
        let c = client(&ep, "sk");
        // Small: one PUT.
        let mut u = c.upload("db/one.gz", "application/gzip");
        u.write_all(b"hello").unwrap();
        assert_eq!(u.finish().unwrap(), 5);
        assert_eq!(c.head("db/one.gz").unwrap(), Some(5));
        {
            let s = seen.lock().unwrap();
            let put = s.iter().find(|r| r.method == "PUT").unwrap();
            assert_eq!(put.target, "/backups/db/one.gz");
            assert_eq!(put.body, b"hello");
        }
        seen.lock().unwrap().clear();
        // Larger than a part: create, parts, complete.
        let data: Vec<u8> = (0..25u32).map(|i| i as u8).collect();
        let mut u = c.upload("db/a b.gz", "application/gzip").part_size(10);
        for chunk in data.chunks(7) {
            u.write_all(chunk).unwrap();
        }
        assert_eq!(u.finish().unwrap(), 25);
        let s = seen.lock().unwrap().clone();
        let calls: Vec<String> = s
            .iter()
            .map(|r| format!("{} {}", r.method, r.target))
            .collect();
        assert_eq!(
            calls,
            [
                "POST /backups/db/a%20b.gz?uploads",
                "PUT /backups/db/a%20b.gz?partNumber=1&uploadId=up-1",
                "PUT /backups/db/a%20b.gz?partNumber=2&uploadId=up-1",
                "PUT /backups/db/a%20b.gz?partNumber=3&uploadId=up-1",
                "POST /backups/db/a%20b.gz?uploadId=up-1",
            ]
        );
        assert_eq!(s[1].body.len(), 10);
        assert_eq!(s[3].body.len(), 5);
        assert!(
            s[0].headers
                .iter()
                .any(|(k, v)| k == "content-type" && v == "application/gzip")
        );
        let complete = String::from_utf8(s[4].body.clone()).unwrap();
        assert_eq!(
            complete,
            complete_body(&[
                (1, "\"etag-1\"".into()),
                (2, "\"etag-2\"".into()),
                (3, "\"etag-3\"".into())
            ])
        );
        // The object reads back whole, and lists.
        let (len, mut r) = c.get("db/a b.gz").unwrap();
        let mut back = Vec::new();
        r.read_to_end(&mut back).unwrap();
        assert_eq!((len, back), (25, data));
        let keys: Vec<String> = c.list("db/").unwrap().into_iter().map(|o| o.key).collect();
        assert_eq!(keys, ["db/a b.gz", "db/one.gz"]);
        c.delete("db/one.gz").unwrap();
        assert_eq!(c.head("db/one.gz").unwrap(), None);
    }

    #[test]
    fn failures_abort_and_report() {
        let (ep, seen) = fake_s3("sk");
        // A wrong secret: S3's error code comes back.
        let bad = client(&ep, "wrong");
        let e = bad.put("k", b"x").unwrap_err().to_string();
        assert!(
            e.contains("SignatureDoesNotMatch") && e.contains("403"),
            "{e}"
        );
        // A multipart upload dropped unfinished is aborted.
        let c = client(&ep, "sk");
        seen.lock().unwrap().clear();
        {
            let mut u = c.upload("big", "application/gzip").part_size(4);
            u.write_all(b"12345678").unwrap();
        }
        let s = seen.lock().unwrap();
        assert_eq!(s.last().unwrap().method, "DELETE");
        assert!(s.last().unwrap().target.contains("uploadId=up-1"));
    }
}
