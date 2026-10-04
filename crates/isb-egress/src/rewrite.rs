//! Putting real secret values on the wire in place of placeholders, and
//! taking them back out of what comes back.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// One secret: what the guest holds, and the real value.
#[derive(Clone)]
pub struct Pair {
    pub placeholder: Vec<u8>,
    pub real: Vec<u8>,
}

impl std::fmt::Debug for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the value.
        write!(f, "Pair({} bytes)", self.real.len())
    }
}

/// Whether a value may travel in a header: no line breaks, no NUL.
pub fn header_safe(v: &[u8]) -> bool {
    !v.iter().any(|b| matches!(b, b'\r' | b'\n' | 0))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Replace every `from` in `hay` with `to`; `None` when there was none.
pub fn replace_all(hay: &[u8], from: &[u8], to: &[u8]) -> Option<Vec<u8>> {
    find(hay, from)?;
    let mut out = Vec::with_capacity(hay.len());
    let mut rest = hay;
    while let Some(i) = find(rest, from) {
        out.extend_from_slice(&rest[..i]);
        out.extend_from_slice(to);
        rest = &rest[i + from.len()..];
    }
    out.extend_from_slice(rest);
    Some(out)
}

/// Percent-encode everything but RFC 3986 unreserved characters.
pub fn url_encode(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    for &b in v {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
    }
    out
}

/// A header value with placeholders replaced by real values: plain
/// substitution, and inside `Basic` credentials (base64 of `user:secret`).
pub fn request_header(value: &[u8], pairs: &[Pair]) -> Vec<u8> {
    let mut v = value.to_vec();
    if let Some(rest) = strip_basic(&v) {
        if let Ok(decoded) = STANDARD.decode(rest.trim_ascii()) {
            let mut d = decoded.clone();
            for p in pairs {
                if let Some(n) = replace_all(&d, &p.placeholder, &p.real) {
                    d = n;
                }
            }
            if d != decoded {
                let mut out = b"Basic ".to_vec();
                out.extend_from_slice(STANDARD.encode(&d).as_bytes());
                v = out;
            }
        }
    }
    for p in pairs {
        if let Some(n) = replace_all(&v, &p.placeholder, &p.real) {
            v = n;
        }
    }
    v
}

fn strip_basic(v: &[u8]) -> Option<&[u8]> {
    (v.len() > 6 && v[..6].eq_ignore_ascii_case(b"basic ")).then(|| &v[6..])
}

/// The request target with placeholders replaced by the percent-encoded
/// real value.
pub fn request_target(target: &[u8], pairs: &[Pair]) -> Vec<u8> {
    let mut v = target.to_vec();
    for p in pairs {
        if let Some(n) = replace_all(&v, &p.placeholder, &url_encode(&p.real)) {
            v = n;
        }
    }
    v
}

/// What to look for in a response, and what to put there instead: the
/// real value raw and percent-encoded, and in Basic credentials.
pub fn scrub_patterns(pairs: &[Pair]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for p in pairs {
        if p.real.is_empty() {
            continue;
        }
        out.push((p.real.clone(), p.placeholder.clone()));
        let enc = url_encode(&p.real);
        if enc != p.real {
            out.push((enc, p.placeholder.clone()));
        }
    }
    // Longest first, so a value that contains another is not half replaced.
    out.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    out
}

/// A header value (of a response) with real values turned back into
/// placeholders.
pub fn response_header(value: &[u8], patterns: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut v = value.to_vec();
    for (from, to) in patterns {
        if let Some(n) = replace_all(&v, from, to) {
            v = n;
        }
    }
    v
}

/// Replaces secrets in a stream of chunks, including one split across
/// chunk boundaries: it holds back the last `longest - 1` bytes until the
/// next chunk (or `finish`) shows whether they begin a match.
pub struct Scrubber {
    patterns: Vec<(Vec<u8>, Vec<u8>)>,
    hold: usize,
    carry: Vec<u8>,
}

impl Scrubber {
    pub fn new(patterns: Vec<(Vec<u8>, Vec<u8>)>) -> Scrubber {
        let hold = patterns
            .iter()
            .map(|(f, _)| f.len())
            .max()
            .unwrap_or(1)
            .saturating_sub(1);
        Scrubber {
            patterns,
            hold,
            carry: Vec::new(),
        }
    }

    pub fn is_noop(&self) -> bool {
        self.patterns.is_empty()
    }

    fn matching(&self, at: &[u8]) -> Option<&(Vec<u8>, Vec<u8>)> {
        self.patterns.iter().find(|(f, _)| at.starts_with(f))
    }

    fn run(&mut self, data: &[u8], last: bool) -> Vec<u8> {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        // Past `limit` a match could still be cut short by the end of `buf`.
        let limit = if last {
            buf.len()
        } else {
            buf.len().saturating_sub(self.hold)
        };
        while i < limit {
            match self.matching(&buf[i..]) {
                Some((from, to)) => {
                    out.extend_from_slice(to);
                    i += from.len();
                }
                None => {
                    out.push(buf[i]);
                    i += 1;
                }
            }
        }
        self.carry = buf[i.min(buf.len())..].to_vec();
        out
    }

    /// Feed a chunk; returns what is safe to send.
    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        if self.patterns.is_empty() {
            return data.to_vec();
        }
        self.run(data, false)
    }

    /// The end of the stream: whatever was held back.
    pub fn finish(&mut self) -> Vec<u8> {
        if self.patterns.is_empty() {
            return Vec::new();
        }
        self.run(&[], true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> Pair {
        Pair {
            placeholder: b"isb_placeholder_abc".to_vec(),
            real: b"s3cr3t/value+x".to_vec(),
        }
    }

    #[test]
    fn headers_get_the_real_value() {
        let p = [pair()];
        assert_eq!(
            request_header(b"Bearer isb_placeholder_abc", &p),
            b"Bearer s3cr3t/value+x"
        );
        assert_eq!(request_header(b"nothing here", &p), b"nothing here");
        assert_eq!(
            request_header(b"a=isb_placeholder_abc; b=isb_placeholder_abc", &p),
            b"a=s3cr3t/value+x; b=s3cr3t/value+x"
        );
    }

    #[test]
    fn basic_credentials_are_decoded_substituted_and_encoded() {
        let p = [pair()];
        let cred = STANDARD.encode(b"user:isb_placeholder_abc");
        let out = request_header(format!("Basic {cred}").as_bytes(), &p);
        let want = STANDARD.encode(b"user:s3cr3t/value+x");
        assert_eq!(out, format!("Basic {want}").as_bytes());
        let other = format!("Basic {}", STANDARD.encode(b"user:pass"));
        assert_eq!(request_header(other.as_bytes(), &p), other.as_bytes());
    }

    #[test]
    fn the_target_gets_the_value_percent_encoded() {
        let p = [pair()];
        assert_eq!(
            request_target(b"/v1?key=isb_placeholder_abc&x=1", &p),
            b"/v1?key=s3cr3t%2Fvalue%2Bx&x=1"
        );
    }

    #[test]
    fn response_headers_lose_the_real_value() {
        let pats = scrub_patterns(&[pair()]);
        assert_eq!(
            response_header(b"token=s3cr3t/value+x", &pats),
            b"token=isb_placeholder_abc"
        );
        assert_eq!(
            response_header(b"/x?k=s3cr3t%2Fvalue%2Bx", &pats),
            b"/x?k=isb_placeholder_abc"
        );
    }

    #[test]
    fn the_scrubber_replaces_a_value_split_across_chunks() {
        let pats = scrub_patterns(&[pair()]);
        let body = b"{\"auth\": \"Bearer s3cr3t/value+x\", \"again\": \"s3cr3t/value+x\"}";
        for cut in 0..body.len() {
            let mut s = Scrubber::new(pats.clone());
            let mut out = s.feed(&body[..cut]);
            out.extend(s.feed(&body[cut..]));
            out.extend(s.finish());
            let text = String::from_utf8(out).unwrap();
            assert!(!text.contains("s3cr3t"), "cut at {cut}: {text}");
            assert_eq!(
                text.matches("isb_placeholder_abc").count(),
                2,
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn a_short_tail_is_released_at_the_end() {
        let mut s = Scrubber::new(scrub_patterns(&[pair()]));
        let mut out = s.feed(b"abc s3cr3t");
        out.extend(s.finish());
        assert_eq!(out, b"abc s3cr3t");
    }

    #[test]
    fn no_patterns_pass_everything_through() {
        let mut s = Scrubber::new(vec![]);
        assert!(s.is_noop());
        assert_eq!(s.feed(b"x"), b"x");
        assert!(s.finish().is_empty());
    }

    #[test]
    fn unsafe_header_values_are_caught() {
        assert!(header_safe(b"abc"));
        assert!(!header_safe(b"a\r\nX: y"));
        assert!(!header_safe(b"a\0b"));
    }

    #[test]
    fn debug_never_prints_the_value() {
        assert!(!format!("{:?}", pair()).contains("s3cr3t"));
    }
}
