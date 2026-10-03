//! SSH public keys on isb accounts: what `isb ssh-proxy` lets into an
//! instance (docs/guides/ssh.md).
//!
//! A key is stored as its algorithm and base64 blob only, re-validated on
//! the way in: no `authorized_keys` options (`command=`, `from=`), no
//! comment, nothing that could add a line or an option when it is written
//! into an instance. Its comment becomes the key's name. The fingerprint is
//! OpenSSH's (`SHA256:` and unpadded base64 of the blob's SHA-256), so it
//! matches what `ssh-keygen -lf` and sshd's log print.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{AuthError, AuthResult, AuthStore};

/// The key types accepted: OpenSSH's current ones. DSA is long gone.
pub const ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "ssh-rsa",
];

/// How many keys one account may hold.
pub const MAX_KEYS: i64 = 50;

/// A parsed public key: `algorithm blob`, and the comment it came with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub algorithm: String,
    /// Standard base64 of the wire-format key.
    pub blob: String,
    pub comment: String,
}

impl PublicKey {
    /// One `authorized_keys`/`known_hosts`-style line, or what is wrong
    /// with it. Options before the key are refused, not dropped: a key
    /// pasted with `command="..."` is not the key its owner thinks it is.
    pub fn parse(line: &str) -> std::result::Result<PublicKey, String> {
        let line = line.trim();
        if line.contains(['\n', '\r', '\0']) {
            return Err("one key per line".into());
        }
        let mut parts = line.split_ascii_whitespace();
        let algorithm = parts.next().ok_or("empty key")?;
        if !ALGORITHMS.contains(&algorithm) {
            return Err(if algorithm.contains('=') || algorithm.contains('"') {
                "authorized_keys options (command=, from=, ...) are not accepted; paste the key alone"
                    .into()
            } else {
                format!(
                    "unsupported key type {:?}; use one of {}",
                    algorithm.chars().take(40).collect::<String>(),
                    ALGORITHMS.join(", ")
                )
            });
        }
        let b64 = parts.next().ok_or("the key's base64 part is missing")?;
        let blob = STANDARD
            .decode(b64)
            .map_err(|_| "the key's base64 part does not decode".to_string())?;
        // The blob starts with its own type as an SSH string.
        let named = wire_string(&blob).ok_or("the key's data is malformed")?;
        if named != algorithm.as_bytes() {
            return Err("the key's type and its data disagree".into());
        }
        if blob.len() > 16 * 1024 {
            return Err("the key is implausibly large".into());
        }
        if algorithm == "ssh-rsa" && blob.len() < 270 {
            return Err("RSA keys under 2048 bits are not accepted".into());
        }
        let comment: String = parts
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect();
        Ok(PublicKey {
            algorithm: algorithm.to_string(),
            blob: STANDARD.encode(&blob),
            comment,
        })
    }

    /// `algorithm blob`, nothing else: what is written into an instance.
    pub fn line(&self) -> String {
        format!("{} {}", self.algorithm, self.blob)
    }

    /// OpenSSH's fingerprint: `SHA256:` and unpadded base64.
    pub fn fingerprint(&self) -> String {
        let blob = STANDARD.decode(&self.blob).unwrap_or_default();
        fingerprint_of(&blob)
    }
}

/// The OpenSSH SHA256 fingerprint of a wire-format key.
pub fn fingerprint_of(blob: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, blob);
    format!("SHA256:{}", STANDARD_NO_PAD.encode(d.as_ref()))
}

/// The first SSH string (u32 length, bytes) of `b`.
fn wire_string(b: &[u8]) -> Option<&[u8]> {
    let n = u32::from_be_bytes(b.get(..4)?.try_into().ok()?) as usize;
    b.get(4..4usize.checked_add(n)?)
}

/// A key on an account, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshKey {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub algorithm: String,
    /// `algorithm blob`.
    pub public_key: String,
    pub fingerprint: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
}

const COLS: &str = "id, user_id, name, algorithm, public_key, fingerprint, created_at, last_used";

fn row(r: &Row) -> rusqlite::Result<SshKey> {
    Ok(SshKey {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        algorithm: r.get(3)?,
        public_key: r.get(4)?,
        fingerprint: r.get(5)?,
        created_at: r.get(6)?,
        last_used: r.get(7)?,
    })
}

impl AuthStore {
    /// Add a public key to `user_id`'s account. `name` defaults to the key's
    /// comment.
    pub fn add_ssh_key(&self, user_id: i64, key: &str, name: Option<&str>) -> AuthResult<SshKey> {
        let k = PublicKey::parse(key).map_err(AuthError::Invalid)?;
        let name = super::clean_name(name.unwrap_or(&k.comment))?;
        let fp = k.fingerprint();
        let db = self.db();
        let n: i64 = db.query_row(
            "SELECT COUNT(*) FROM ssh_keys WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        )?;
        if n >= MAX_KEYS {
            return Err(AuthError::Invalid(format!(
                "an account holds at most {MAX_KEYS} SSH keys; remove one first"
            )));
        }
        let r = db.execute(
            "INSERT INTO ssh_keys (user_id, name, algorithm, public_key, fingerprint, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, name, k.algorithm, k.line(), fp, self.now()],
        );
        match r {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(AuthError::Conflict(format!(
                    "that key ({fp}) is already on this account"
                )));
            }
            Err(e) => return Err(e.into()),
        }
        let id = db.last_insert_rowid();
        Ok(db.query_row(
            &format!("SELECT {COLS} FROM ssh_keys WHERE id = ?1"),
            [id],
            row,
        )?)
    }

    pub fn list_ssh_keys(&self, user_id: i64) -> AuthResult<Vec<SshKey>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {COLS} FROM ssh_keys WHERE user_id = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([user_id], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ssh_key(&self, user_id: i64, id: i64) -> AuthResult<Option<SshKey>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {COLS} FROM ssh_keys WHERE id = ?1 AND user_id = ?2"),
                params![id, user_id],
                row,
            )
            .optional()?)
    }

    /// Remove one of `user_id`'s keys; `false` if they have no such key.
    pub fn delete_ssh_key(&self, user_id: i64, id: i64) -> AuthResult<bool> {
        Ok(self.db().execute(
            "DELETE FROM ssh_keys WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )? > 0)
    }

    /// Is `fingerprint` still one of `user_id`'s keys? Asked during a live
    /// session, so removing a key ends the sessions it opened.
    pub fn has_ssh_key(&self, user_id: i64, fingerprint: &str) -> AuthResult<bool> {
        Ok(self
            .db()
            .query_row(
                "SELECT 1 FROM ssh_keys WHERE user_id = ?1 AND fingerprint = ?2",
                params![user_id, fingerprint],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Note that a key just opened a session.
    pub fn touch_ssh_key(&self, user_id: i64, fingerprint: &str) -> AuthResult<()> {
        self.db().execute(
            "UPDATE ssh_keys SET last_used = ?3 WHERE user_id = ?1 AND fingerprint = ?2",
            params![user_id, fingerprint, self.now()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ssh-keygen -t ed25519` output (a throwaway key).
    const ED: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh alice@laptop";

    #[test]
    fn parses_and_fingerprints_like_openssh() {
        let k = PublicKey::parse(ED).unwrap();
        assert_eq!(k.algorithm, "ssh-ed25519");
        assert_eq!(k.comment, "alice@laptop");
        assert_eq!(
            k.line(),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh"
        );
        // As `ssh-keygen -lf` prints it.
        assert_eq!(
            k.fingerprint(),
            "SHA256:OGav3hSvMQSOfHiDB0OdyYFOPHDbUJTzSsNvCdLadvQ"
        );
        // Surrounding whitespace and a missing comment are fine.
        let bare = PublicKey::parse(&format!("  {}  ", k.line())).unwrap();
        assert_eq!(bare.comment, "");
        assert_eq!(bare.fingerprint(), k.fingerprint());
    }

    #[test]
    fn refuses_options_garbage_and_mismatches() {
        for bad in [
            "",
            "ssh-ed25519",
            "ssh-ed25519 !!!notbase64",
            "ssh-dss AAAAB3NzaC1kc3MAAACBAP",
            &format!("command=\"rm -rf /\" {ED}"),
            &format!("no-pty {ED}"),
            &format!("{ED}\nssh-ed25519 AAAA"),
            // An ed25519 blob labelled as ecdsa.
            "ecdsa-sha2-nistp256 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh",
            // A truncated blob.
            "ssh-ed25519 AAAAC3NzaC1lZDI1",
            // RSA-1024.
            "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQCh+Ulvy6UEPNQQAlurADNGZXtfgFgks7nw6P1Esxf7DoMJyFmmmPnc3sVISBq2LusPE56vSNx5iRXIVe3YtacPtgmzPeNqM5cDILvQutXw3uwsGv6EnnALiDP6pluMnPJ1uKo318it/Dzgatzzmsl2qc4ZiC2e5IRTkto1nPjxfw==",
        ] {
            assert!(PublicKey::parse(bad).is_err(), "{bad:?}");
        }
        let e = PublicKey::parse(&format!("command=\"x\" {ED}")).unwrap_err();
        assert!(e.contains("options"), "{e}");
    }

    #[test]
    fn stores_lists_and_removes() {
        let s = AuthStore::in_memory(Default::default()).unwrap();
        let u = s
            .create_first_admin("a@example.com", "A", "pw-long-enough-1")
            .unwrap();
        let k = s.add_ssh_key(u.id, ED, None).unwrap();
        assert_eq!(k.name, "alice@laptop");
        assert!(s.has_ssh_key(u.id, &k.fingerprint).unwrap());
        assert!(matches!(
            s.add_ssh_key(u.id, ED, Some("again")),
            Err(AuthError::Conflict(_))
        ));
        s.touch_ssh_key(u.id, &k.fingerprint).unwrap();
        let l = s.list_ssh_keys(u.id).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].last_used.is_some());
        // Someone else's id is not yours to remove.
        assert!(!s.delete_ssh_key(u.id + 1, k.id).unwrap());
        assert!(s.delete_ssh_key(u.id, k.id).unwrap());
        assert!(!s.has_ssh_key(u.id, &k.fingerprint).unwrap());
        assert!(s.list_ssh_keys(u.id).unwrap().is_empty());
    }
}
