//! The dead man's switch: a GET to an outside URL every interval
//! (healthchecks.io, Better Stack, Uptime Kuma's push monitors, cronitor,
//! or anything that alerts when the pings stop), so someone hears about it
//! when the host, incusd's machine or the daemon itself is gone, which no
//! event from the daemon can say.
//!
//! The URL is the operator's (`--heartbeat-url`, better
//! `ISB_HEARTBEAT_URL`), so the address policy does not apply; its path
//! often holds the check's token, so logs name only its host.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::probe::{self, HttpProbe};
use crate::net;

/// A heartbeat's settings.
#[derive(Debug, Clone)]
pub struct Heartbeat {
    pub url: String,
    pub interval: Duration,
}

impl Heartbeat {
    /// Check the URL (it is never printed) and the interval.
    pub fn new(url: &str, interval: Duration) -> Result<Heartbeat, String> {
        net::parse_url(url).map_err(|e| format!("--heartbeat-url: {e}"))?;
        if interval < Duration::from_secs(10) || interval > Duration::from_secs(3600) {
            return Err("--heartbeat-interval: 10s to 1h".into());
        }
        Ok(Heartbeat {
            url: url.trim().to_string(),
            interval,
        })
    }

    /// The host pings go to, for logs.
    pub fn host(&self) -> String {
        net::parse_url(&self.url)
            .map(|t| t.host)
            .unwrap_or_default()
    }

    /// Ping once: `Ok(status)` for a 2xx or 3xx answer.
    pub fn ping(&self) -> Result<u16, String> {
        let a = probe::http(&HttpProbe {
            url: self.url.clone(),
            method: "GET".into(),
            headers: Vec::new(),
            timeout: Duration::from_secs(10).min(self.interval / 2),
            follow_redirects: true,
            allow_private: true,
            connect_to: None,
            tls: net::default_tls(),
        })?;
        if (200..400).contains(&a.status) {
            Ok(a.status)
        } else {
            Err(format!("HTTP {}", a.status))
        }
    }

    /// Ping now and every interval until `stop`, logging when it starts
    /// and stops working.
    pub fn start(self, stop: Arc<AtomicBool>) {
        eprintln!(
            "isb serve: heartbeat to {} every {}s",
            self.host(),
            self.interval.as_secs()
        );
        let _ = std::thread::Builder::new()
            .name("isb-heartbeat".into())
            .spawn(move || {
                let mut ok = None;
                while !stop.load(Ordering::SeqCst) {
                    let started = Instant::now();
                    let r = self.ping();
                    match (&r, ok) {
                        (Ok(_), Some(true)) | (Err(_), Some(false)) => {}
                        (Ok(s), _) => {
                            eprintln!("isb serve: heartbeat to {}: ok (HTTP {s})", self.host())
                        }
                        (Err(e), _) => {
                            eprintln!("isb serve: heartbeat to {}: failing: {e}", self.host())
                        }
                    }
                    ok = Some(r.is_ok());
                    while started.elapsed() < self.interval && !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn pings_and_reports() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut got = Vec::new();
            for code in [200, 500] {
                let (mut s, _) = l.accept().unwrap();
                let mut b = [0u8; 2048];
                let n = s.read(&mut b).unwrap();
                got.push(String::from_utf8_lossy(&b[..n]).to_string());
                write!(s, "HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n").unwrap();
            }
            got
        });
        let hb = Heartbeat::new(
            &format!("http://127.0.0.1:{port}/ping/abc-123"),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(hb.host(), "127.0.0.1");
        assert_eq!(hb.ping(), Ok(200));
        assert_eq!(hb.ping(), Err("HTTP 500".into()));
        assert!(h.join().unwrap()[0].starts_with("GET /ping/abc-123 HTTP/1.1"));
        assert!(Heartbeat::new("ftp://x", Duration::from_secs(30)).is_err());
        assert!(Heartbeat::new("https://hc-ping.com/x", Duration::from_secs(1)).is_err());
    }
}
