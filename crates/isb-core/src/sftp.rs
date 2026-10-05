//! Just enough SFTP (version 3) to stat, chown and chmod a path inside an
//! instance through incus' `/1.0/instances/NAME/sftp`.
//!
//! incusd serves it from inside the instance's own namespaces (forkfile, or
//! the agent in a VM), so ids are the guest's own whatever the idmap, and the
//! guest needs no shell or tool at all.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};

const INIT: u8 = 1;
const VERSION: u8 = 2;
const SETSTAT: u8 = 9;
const STAT: u8 = 17;
const STATUS: u8 = 101;
const ATTRS: u8 = 105;
const ATTR_SIZE: u32 = 0x1;
const ATTR_UIDGID: u32 = 0x2;
const ATTR_PERMISSIONS: u32 = 0x4;
#[cfg(test)]
const ATTR_ACMODTIME: u32 = 0x8;
const NO_SUCH_FILE: u32 = 2;

/// Owner and mode of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits (`0o7777`), the file type masked off.
    pub mode: u32,
}

/// One SFTP session with an instance.
pub struct Sftp {
    stream: UnixStream,
    next_id: u32,
    what: String,
}

impl Sftp {
    /// Open a session; every request then waits at most `timeout`.
    pub fn open(client: &Client, instance: &str, timeout: Duration) -> Result<Self> {
        let path = format!("/1.0/instances/{}/sftp", encode_segment(instance));
        let stream = client.upgrade(&path, "sftp", timeout)?;
        let mut s = Sftp {
            stream,
            next_id: 1,
            what: format!("sftp to {instance}"),
        };
        s.send(INIT, &3u32.to_be_bytes())?;
        let (t, _) = s.recv()?;
        if t != VERSION {
            return Err(s.protocol(format!("expected VERSION, got packet type {t}")));
        }
        Ok(s)
    }

    /// stat (following symlinks); `None` if nothing is there.
    pub fn stat(&mut self, path: &str) -> Result<Option<Stat>> {
        let id = self.request(STAT, path, &[])?;
        let (t, body) = self.recv()?;
        let mut r = Reader::new(&body);
        if r.u32()? != id {
            return Err(self.protocol("reply to another request".into()));
        }
        match t {
            ATTRS => {
                let flags = r.u32()?;
                if flags & ATTR_SIZE != 0 {
                    r.u64()?;
                }
                if flags & ATTR_UIDGID == 0 || flags & ATTR_PERMISSIONS == 0 {
                    return Err(self.protocol(format!("stat {path}: no owner or mode")));
                }
                let (uid, gid) = (r.u32()?, r.u32()?);
                let mode = r.u32()? & 0o7777;
                Ok(Some(Stat { uid, gid, mode }))
            }
            STATUS => match r.u32()? {
                NO_SUCH_FILE => Ok(None),
                code => Err(self.failed(&format!("stat {path}"), code, &mut r)),
            },
            t => Err(self.protocol(format!("unexpected packet type {t}"))),
        }
    }

    /// chown (following symlinks).
    pub fn chown(&mut self, path: &str, uid: u32, gid: u32) -> Result<()> {
        let mut a = ATTR_UIDGID.to_be_bytes().to_vec();
        a.extend(uid.to_be_bytes());
        a.extend(gid.to_be_bytes());
        self.setstat(path, &a, &format!("chown {uid}:{gid} {path}"))
    }

    /// chmod to `mode` (`0o7777` bits).
    pub fn chmod(&mut self, path: &str, mode: u32) -> Result<()> {
        let mut a = ATTR_PERMISSIONS.to_be_bytes().to_vec();
        a.extend((mode & 0o7777).to_be_bytes());
        self.setstat(path, &a, &format!("chmod {mode:04o} {path}"))
    }

    fn setstat(&mut self, path: &str, attrs: &[u8], what: &str) -> Result<()> {
        let id = self.request(SETSTAT, path, attrs)?;
        let (t, body) = self.recv()?;
        let mut r = Reader::new(&body);
        if t != STATUS || r.u32()? != id {
            return Err(self.protocol(format!("{what}: unexpected reply type {t}")));
        }
        match r.u32()? {
            0 => Ok(()),
            code => Err(self.failed(what, code, &mut r)),
        }
    }

    fn request(&mut self, kind: u8, path: &str, rest: &[u8]) -> Result<u32> {
        let id = self.next_id;
        self.next_id += 1;
        let mut body = id.to_be_bytes().to_vec();
        body.extend((path.len() as u32).to_be_bytes());
        body.extend(path.as_bytes());
        body.extend(rest);
        self.send(kind, &body)?;
        Ok(id)
    }

    fn send(&mut self, kind: u8, body: &[u8]) -> Result<()> {
        let mut p = ((body.len() + 1) as u32).to_be_bytes().to_vec();
        p.push(kind);
        p.extend(body);
        self.stream.write_all(&p).map_err(|e| self.io(e))
    }

    fn recv(&mut self) -> Result<(u8, Vec<u8>)> {
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len).map_err(|e| self.io(e))?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 || len > 1 << 20 {
            return Err(self.protocol(format!("packet of {len} bytes")));
        }
        let mut p = vec![0u8; len];
        self.stream.read_exact(&mut p).map_err(|e| self.io(e))?;
        Ok((p[0], p.split_off(1)))
    }

    fn failed(&self, what: &str, code: u32, r: &mut Reader) -> Error {
        let msg = r.string().unwrap_or_default();
        Error::OperationFailed {
            step: format!("{} ({what})", self.what),
            message: if msg.is_empty() {
                format!("SFTP status {code}")
            } else {
                msg
            },
        }
    }

    fn protocol(&self, m: String) -> Error {
        Error::Protocol(format!("{}: {m}", self.what))
    }

    fn io(&self, e: std::io::Error) -> Error {
        Error::Protocol(format!("{}: {e}", self.what))
    }
}

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.b.len() < n {
            return Err(Error::Protocol("sftp: short packet".into()));
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(((self.u32()? as u64) << 32) | self.u32()? as u64)
    }

    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_attrs_as_incus_sends_them() {
        // An ATTRS reply captured from incus 7.5 (size, uid/gid, mode, times).
        let body = b"\x00\x00\x00\x01\x00\x00\x00\x0f\x00\x00\x00\x00\x00\x00\x10\x00\
            \x00\x00\x03\xe8\x00\x00\x03\xe9\x00\x00\x41\xc9\x6a\xc3\x3f\x51\x6a\xc3\x3f\x51";
        let mut r = Reader::new(body);
        assert_eq!(r.u32().unwrap(), 1);
        let flags = r.u32().unwrap();
        assert_eq!(
            flags,
            ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS | ATTR_ACMODTIME
        );
        assert_eq!(r.u64().unwrap(), 4096);
        assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (1000, 1001));
        assert_eq!(r.u32().unwrap() & 0o7777, 0o711);
        assert!(Reader::new(b"\x00\x00").u32().is_err());
    }
}
