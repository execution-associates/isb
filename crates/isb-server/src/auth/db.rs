//! The identity database: SQLite in WAL mode, schema migrations recorded in
//! `schema_version`.
//!
//! Times are unix seconds. Token columns hold SHA-256 hashes, never tokens.
//! Roles are text, validated in Rust ([`super::Role`]), so a new role needs
//! no migration.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::AuthError;

/// Each entry moves the schema from version `i` to `i + 1`. Append only:
/// never edit a migration that has shipped.
const MIGRATIONS: &[&str] = &[
    // 1: users, identities, orgs, memberships, sessions, invitations, API
    // tokens, password resets.
    "
    CREATE TABLE users (
        id             INTEGER PRIMARY KEY,
        email          TEXT NOT NULL UNIQUE COLLATE NOCASE,
        name           TEXT NOT NULL DEFAULT '',
        password_hash  TEXT,
        platform_admin INTEGER NOT NULL DEFAULT 0,
        created_at     INTEGER NOT NULL,
        disabled       INTEGER NOT NULL DEFAULT 0
    );
    -- External sign-in methods (OAuth, OIDC): one row per provider account.
    CREATE TABLE user_identities (
        id             INTEGER PRIMARY KEY,
        user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        provider       TEXT NOT NULL,
        subject        TEXT NOT NULL,
        email          TEXT,
        email_verified INTEGER NOT NULL DEFAULT 0,
        created_at     INTEGER NOT NULL,
        last_used      INTEGER,
        UNIQUE (provider, subject)
    );
    CREATE INDEX user_identities_user ON user_identities(user_id);
    CREATE TABLE orgs (
        name       TEXT PRIMARY KEY,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE memberships (
        user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        org        TEXT NOT NULL REFERENCES orgs(name) ON DELETE CASCADE,
        role       TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_id, org)
    );
    CREATE INDEX memberships_org ON memberships(org);
    CREATE TABLE sessions (
        id         INTEGER PRIMARY KEY,
        token_hash BLOB NOT NULL UNIQUE,
        user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        last_seen  INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        user_agent TEXT,
        ip         TEXT
    );
    CREATE INDEX sessions_user ON sessions(user_id);
    CREATE TABLE invitations (
        id          INTEGER PRIMARY KEY,
        token_hash  BLOB NOT NULL UNIQUE,
        org         TEXT NOT NULL REFERENCES orgs(name) ON DELETE CASCADE,
        email       TEXT NOT NULL COLLATE NOCASE,
        role        TEXT NOT NULL,
        invited_by  INTEGER REFERENCES users(id) ON DELETE SET NULL,
        created_at  INTEGER NOT NULL,
        expires_at  INTEGER NOT NULL,
        accepted_at INTEGER,
        accepted_by INTEGER REFERENCES users(id) ON DELETE SET NULL
    );
    CREATE INDEX invitations_org ON invitations(org);
    CREATE TABLE api_tokens (
        id         INTEGER PRIMARY KEY,
        token_hash BLOB NOT NULL UNIQUE,
        name       TEXT NOT NULL,
        user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        org        TEXT REFERENCES orgs(name) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        last_used  INTEGER,
        expires_at INTEGER
    );
    CREATE INDEX api_tokens_user ON api_tokens(user_id);
    CREATE INDEX api_tokens_org ON api_tokens(org);
    CREATE TABLE password_resets (
        id         INTEGER PRIMARY KEY,
        token_hash BLOB NOT NULL UNIQUE,
        user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        used_at    INTEGER
    );
    ",
    // 2: WebAuthn passkeys. `user_handle` is the random WebAuthn user id
    // (the same for all of a user's passkeys); `public_key` is the COSE_Key.
    "
    CREATE TABLE passkeys (
        id            INTEGER PRIMARY KEY,
        credential_id BLOB NOT NULL UNIQUE,
        user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        user_handle   BLOB NOT NULL,
        public_key    BLOB NOT NULL,
        alg           INTEGER NOT NULL,
        sign_count    INTEGER NOT NULL DEFAULT 0,
        transports    TEXT NOT NULL DEFAULT '[]',
        aaguid        TEXT NOT NULL DEFAULT '',
        name          TEXT NOT NULL DEFAULT '',
        created_at    INTEGER NOT NULL,
        last_used     INTEGER
    );
    CREATE INDEX passkeys_user ON passkeys(user_id);
    ",
    // 3: API token scopes, a JSON array of strings; NULL is the role's
    // whole reach.
    "
    ALTER TABLE api_tokens ADD COLUMN scopes TEXT;
    ",
    // 4: superadmin tokens: the unix socket's reach over HTTP. Nobody's:
    // minted on the host (`isb token create NAME --superadmin`), named
    // uniquely so the audit log's `token:<name>` is unambiguous.
    "
    CREATE TABLE superadmin_tokens (
        id         INTEGER PRIMARY KEY,
        token_hash BLOB NOT NULL UNIQUE,
        name       TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        last_used  INTEGER,
        expires_at INTEGER
    );
    ",
    // 5: SSH public keys on accounts (`isb ssh-proxy` lets them into an
    // instance). `public_key` is `algorithm base64`, nothing else.
    "
    CREATE TABLE ssh_keys (
        id          INTEGER PRIMARY KEY,
        user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        name        TEXT NOT NULL DEFAULT '',
        algorithm   TEXT NOT NULL,
        public_key  TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        last_used   INTEGER,
        UNIQUE (user_id, fingerprint)
    );
    ",
    // 6: agent identities: tailnet logins and tags, Access emails and service
    // token client ids, each mapped to a role in one org (never owner).
    "
    CREATE TABLE org_agent_identities (
        id         INTEGER PRIMARY KEY,
        org        TEXT NOT NULL REFERENCES orgs(name) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        subject    TEXT NOT NULL,
        role       TEXT NOT NULL,
        note       TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        created_by TEXT NOT NULL DEFAULT '',
        UNIQUE (org, kind, subject)
    );
    CREATE INDEX org_agent_identities_subject ON org_agent_identities(kind, subject);
    ",
    // 7: superadmin identities kept as state (`isb superadmin add`), on top
    // of `--superadmin-access` / `--superadmin-tailnet`: tailnet logins and
    // tags, Access emails and service token client ids. Written only on the
    // host, never over HTTP.
    "
    CREATE TABLE superadmin_identities (
        id       INTEGER PRIMARY KEY,
        kind     TEXT NOT NULL,
        value    TEXT NOT NULL,
        added_at INTEGER NOT NULL,
        added_by TEXT NOT NULL DEFAULT '',
        UNIQUE (kind, value)
    );
    ",
];

/// The schema version this build writes.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Open (creating if needed) and migrate. The file is 0600 in a 0700
/// directory: it holds password hashes.
pub fn open(path: &Path) -> Result<Connection, AuthError> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| AuthError::io(format!("create {}", dir.display()), e))?;
        }
    }
    // Create the file 0600 before SQLite does (it would use the umask).
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| AuthError::io(format!("open {}", path.display()), e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| AuthError::io(format!("chmod {}", path.display()), e))?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// An in-memory database, for tests.
pub fn open_in_memory() -> Result<Connection, AuthError> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<(), AuthError> {
    // The daemon and the CLI share the file; a writer waits up to 5s for the
    // other instead of failing with SQLITE_BUSY.
    conn.busy_timeout(Duration::from_secs(5))?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if mode != "wal" && mode != "memory" {
        return Err(AuthError::Internal(format!(
            "identity database: journal_mode is {mode}, wanted wal"
        )));
    }
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA secure_delete = ON;",
    )?;
    Ok(())
}

/// The version recorded in `schema_version`, 0 for a new database.
pub fn version(conn: &Connection) -> Result<i64, AuthError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")?;
    Ok(conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| {
            r.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0))
}

/// Apply every migration past the recorded version, each in its own
/// transaction. A database from a newer isb is refused, not downgraded.
pub fn migrate(conn: &Connection) -> Result<(), AuthError> {
    let mut v = version(conn)?;
    if v > SCHEMA_VERSION {
        return Err(AuthError::Internal(format!(
            "identity database is schema version {v}, newer than this isb understands \
             ({SCHEMA_VERSION}); upgrade isb"
        )));
    }
    while v < SCHEMA_VERSION {
        let sql = MIGRATIONS[v as usize];
        // IMMEDIATE takes the write lock up front, so two processes opening a
        // new database at once cannot both run migration 1.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let r = (|| -> Result<(), AuthError> {
            let now_v = version(conn)?;
            if now_v != v {
                // Someone else migrated while we waited for the lock.
                return Ok(());
            }
            conn.execute_batch(sql)?;
            conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [v + 1])?;
            Ok(())
        })();
        match r {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(AuthError::Internal(format!(
                    "identity database migration {}: {e}",
                    v + 1
                )));
            }
        }
        v = version(conn)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_refuses_newer() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/isb.db");
        let c = open(&p).unwrap();
        assert_eq!(version(&c).unwrap(), SCHEMA_VERSION);
        let mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(c);
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(m, 0o600);
            let d = std::fs::metadata(p.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(d, 0o700);
        }
        // Reopening applies nothing new.
        let c = open(&p).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, SCHEMA_VERSION);
        // A database from the future is refused.
        c.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [SCHEMA_VERSION + 1],
        )
        .unwrap();
        drop(c);
        let e = open(&p).unwrap_err().to_string();
        assert!(e.contains("newer than this isb"), "{e}");
    }

    #[test]
    fn upgrades_a_version_1_database() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("isb.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version VALUES (1);
                 INSERT INTO users (email, created_at) VALUES ('a@x.io', 0);",
            )
            .unwrap();
        }
        let c = open(&p).unwrap();
        assert_eq!(version(&c).unwrap(), SCHEMA_VERSION);
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM passkeys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn email_is_case_insensitive() {
        let c = open_in_memory().unwrap();
        c.execute(
            "INSERT INTO users (email, created_at) VALUES ('A@x.io', 0)",
            [],
        )
        .unwrap();
        assert!(
            c.execute(
                "INSERT INTO users (email, created_at) VALUES ('a@X.IO', 0)",
                [],
            )
            .is_err()
        );
    }
}
