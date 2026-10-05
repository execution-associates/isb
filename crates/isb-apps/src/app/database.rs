//! Databases: apps whose source is an engine and a version.
//!
//! A database is an ordinary app (`source: {database: {engine, version}}`)
//! rendered into its project environment's stack like any other, so other
//! apps reach it by service name (`<db>.<project>-<env>`). What the engine
//! adds over an image app: the official image pinned by version, a named
//! volume for its data, a health check, credentials generated on create and
//! kept as org secrets (delivered as `{secret: NAME}` environment), the
//! native dump and restore commands backups run inside its instance, and
//! connection details that name the password secret, never its value.
//!
//! Databases always roll out stop-first (they have a volume) and run one
//! replica: two writers on one data directory corrupt it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{AppSpec, EnvValue};
use crate::error::{Error, Result};

/// A database engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    #[serde(alias = "mongo")]
    Mongodb,
    Redis,
}

pub const ENGINES: [Engine; 5] = [
    Engine::Postgres,
    Engine::Mysql,
    Engine::Mariadb,
    Engine::Mongodb,
    Engine::Redis,
];

/// The volume a database keeps its data in (`<stack>_<db>_data`).
pub const DATA_VOLUME: &str = "data";

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Mysql => "mysql",
            Engine::Mariadb => "mariadb",
            Engine::Mongodb => "mongodb",
            Engine::Redis => "redis",
        }
    }

    pub fn parse(s: &str) -> Result<Engine> {
        match s.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Ok(Engine::Postgres),
            "mysql" => Ok(Engine::Mysql),
            "mariadb" => Ok(Engine::Mariadb),
            "mongodb" | "mongo" => Ok(Engine::Mongodb),
            "redis" => Ok(Engine::Redis),
            _ => Err(Error::invalid(format!(
                "engine {s:?}: postgres, mysql, mariadb, mongodb or redis"
            ))),
        }
    }

    /// The version a new database gets.
    pub fn default_version(self) -> &'static str {
        match self {
            Engine::Postgres => "17",
            Engine::Mysql => "8.4",
            Engine::Mariadb => "11.4",
            Engine::Mongodb => "8.0",
            Engine::Redis => "7.4",
        }
    }

    /// The official image's repository on Docker Hub.
    fn repository(self) -> &'static str {
        match self {
            Engine::Mongodb => "mongo",
            e => e.name(),
        }
    }

    pub fn port(self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::Mysql | Engine::Mariadb => 3306,
            Engine::Mongodb => 27017,
            Engine::Redis => 6379,
        }
    }

    pub fn data_path(self) -> &'static str {
        match self {
            Engine::Postgres => "/var/lib/postgresql/data",
            Engine::Mysql | Engine::Mariadb => "/var/lib/mysql",
            Engine::Mongodb => "/data/db",
            Engine::Redis => "/data",
        }
    }

    /// Whether a root password is kept beside the user's.
    pub fn has_root_password(self) -> bool {
        matches!(self, Engine::Mysql | Engine::Mariadb)
    }

    /// Whether the engine has a database name and a user to set.
    pub fn has_database(self) -> bool {
        !matches!(self, Engine::Redis)
    }

    /// The URL scheme of a connection string.
    fn scheme(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Mysql | Engine::Mariadb => "mysql",
            Engine::Mongodb => "mongodb",
            Engine::Redis => "redis",
        }
    }

    /// The extension of a dump's object (before compression).
    pub fn dump_extension(self) -> &'static str {
        match self {
            Engine::Postgres => "pgdump",
            Engine::Mysql | Engine::Mariadb => "sql",
            Engine::Mongodb => "archive",
            Engine::Redis => "rdb",
        }
    }

    /// Restore a dump of `from` into this engine?
    pub fn restores_from(self, from: Engine) -> bool {
        self == from
            || matches!(
                (self, from),
                (Engine::Mysql, Engine::Mariadb) | (Engine::Mariadb, Engine::Mysql)
            )
    }

    /// A shell line (run inside the database's instance, as root, with its
    /// environment) that writes a dump to stdout. Credentials come from the
    /// instance's own environment, never from argv the daemon builds.
    pub fn dump_command(self) -> Vec<String> {
        let line = match self {
            Engine::Postgres => {
                r#"PGPASSWORD="$POSTGRES_PASSWORD" exec pg_dump -h 127.0.0.1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" --format=custom -Z0"#
            }
            Engine::Mysql => {
                r#"MYSQL_PWD="$MYSQL_ROOT_PASSWORD" exec mysqldump -h 127.0.0.1 -uroot --single-transaction --routines --triggers --events --no-tablespaces --set-gtid-purged=OFF "$MYSQL_DATABASE""#
            }
            Engine::Mariadb => {
                r#"MYSQL_PWD="$MARIADB_ROOT_PASSWORD" exec mariadb-dump -h 127.0.0.1 -uroot --single-transaction --routines --triggers --events "$MARIADB_DATABASE""#
            }
            Engine::Mongodb => {
                r#"exec mongodump --quiet --archive --host 127.0.0.1 --username "$MONGO_INITDB_ROOT_USERNAME" --password "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin --db "$MONGO_INITDB_DATABASE""#
            }
            // SAVE writes a consistent snapshot; then hand it over.
            Engine::Redis => {
                r#"redis-cli -h 127.0.0.1 --no-auth-warning SAVE >/dev/null && exec cat /data/dump.rdb"#
            }
        };
        vec!["/bin/sh".into(), "-c".into(), line.into()]
    }

    /// The command that makes a new password take effect inside a running
    /// database (`rotate`): the user's (`root: false`), or MySQL's and
    /// MariaDB's root password. The new value arrives on stdin; the
    /// instance's environment still holds the old one, which authenticates.
    /// The engines read their password variables only when the data
    /// directory is first made, so without this a new password would lock
    /// the apps out.
    pub fn rotate_command(self, root: bool) -> Vec<String> {
        // MySQL string literal: backslashes and quotes doubled.
        const MY_ESC: &str = r#"pw=$(cat | sed -e 's/\\/\\\\/g' -e "s/'/''/g")"#;
        let line = match (self, root) {
            (Engine::Postgres, _) => r#"pw=$(cat) && q="'" && printf 'ALTER ROLE :"u" PASSWORD :%spw%s;\n' "$q" "$q" | PGPASSWORD="$POSTGRES_PASSWORD" psql -v ON_ERROR_STOP=1 -q -h 127.0.0.1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -v u="$POSTGRES_USER" -v pw="$pw""#.to_string(),
            (Engine::Mysql, false) => format!(
                r#"{MY_ESC} && printf "ALTER USER '%s'@'%%' IDENTIFIED BY '%s';\n" "$MYSQL_USER" "$pw" | MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql -h 127.0.0.1 -uroot"#
            ),
            (Engine::Mysql, true) => format!(
                r#"{MY_ESC} && printf "ALTER USER IF EXISTS 'root'@'%%' IDENTIFIED BY '%s'; ALTER USER IF EXISTS 'root'@'localhost' IDENTIFIED BY '%s';\n" "$pw" "$pw" | MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql -h 127.0.0.1 -uroot"#
            ),
            (Engine::Mariadb, false) => format!(
                r#"{MY_ESC} && printf "ALTER USER '%s'@'%%' IDENTIFIED BY '%s';\n" "$MARIADB_USER" "$pw" | MYSQL_PWD="$MARIADB_ROOT_PASSWORD" mariadb -h 127.0.0.1 -uroot"#
            ),
            (Engine::Mariadb, true) => format!(
                r#"{MY_ESC} && printf "ALTER USER IF EXISTS 'root'@'%%' IDENTIFIED BY '%s'; ALTER USER IF EXISTS 'root'@'localhost' IDENTIFIED BY '%s';\n" "$pw" "$pw" | MYSQL_PWD="$MARIADB_ROOT_PASSWORD" mariadb -h 127.0.0.1 -uroot"#
            ),
            (Engine::Mongodb, _) => r#"NEW=$(cat) mongosh --quiet --host 127.0.0.1 admin -u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin --eval 'db.changeUserPassword(process.env.MONGO_INITDB_ROOT_USERNAME, process.env.NEW)'"#.to_string(),
            (Engine::Redis, _) => r#"NEW=$(cat) && redis-cli -h 127.0.0.1 --no-auth-warning CONFIG SET requirepass "$NEW" | grep -q OK"#.to_string(),
        };
        vec!["/bin/sh".into(), "-c".into(), line]
    }

    /// A shell line that restores a dump read from stdin, replacing what
    /// the dump holds. `ISB_SOURCE_DB` names the dumped database (MongoDB
    /// renames it to this one's).
    pub fn restore_command(self) -> Vec<String> {
        let line = match self {
            Engine::Postgres => {
                r#"PGPASSWORD="$POSTGRES_PASSWORD" exec pg_restore -h 127.0.0.1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" --clean --if-exists --no-owner --no-privileges"#
            }
            Engine::Mysql => {
                r#"MYSQL_PWD="$MYSQL_ROOT_PASSWORD" exec mysql -h 127.0.0.1 -uroot "$MYSQL_DATABASE""#
            }
            Engine::Mariadb => {
                r#"MYSQL_PWD="$MARIADB_ROOT_PASSWORD" exec mariadb -h 127.0.0.1 -uroot "$MARIADB_DATABASE""#
            }
            Engine::Mongodb => {
                r#"exec mongorestore --quiet --archive --drop --host 127.0.0.1 --username "$MONGO_INITDB_ROOT_USERNAME" --password "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin --nsFrom "$ISB_SOURCE_DB.*" --nsTo "$MONGO_INITDB_DATABASE.*""#
            }
            // Replace the snapshot and stop the server without saving over
            // it; the stack controller starts it again, loading the dump.
            Engine::Redis => {
                r#"cat > /data/dump.rdb.isb-restore && mv /data/dump.rdb.isb-restore /data/dump.rdb && chown redis:redis /data/dump.rdb 2>/dev/null; redis-cli -h 127.0.0.1 --no-auth-warning SHUTDOWN NOSAVE >/dev/null 2>&1; exit 0"#
            }
        };
        vec!["/bin/sh".into(), "-c".into(), line.into()]
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// `source: {database: ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseSource {
    pub engine: Engine,
    /// The image tag (`17`, `8.4`); default per engine. Changing it is a
    /// deploy of the new image over the same data: minor versions only for
    /// engines that cannot upgrade data in place (Postgres majors).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// The database created on first start (not Redis). Default: the app
    /// name with `-` as `_`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// The user created on first start (not Redis; MongoDB's root user).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// More secrets isb keeps holding the internal URL, each with its own
    /// query string (`{"dsn.chat-postgres.mattermost": "sslmode=disable"}`,
    /// `""` for none): written at deploy and again when the password
    /// changes, so an app that needs driver options never holds a copy of
    /// the password that goes stale.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub urls: BTreeMap<String, String>,
}

/// A database or user name: an identifier every engine takes unquoted.
fn validate_ident(what: &str, s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{what} {s:?}: up to 63 characters of [A-Za-z0-9_], not starting with a digit"
        )))
    }
}

fn validate_version(v: &str) -> Result<()> {
    let ok = !v.is_empty()
        && v.len() <= 64
        && v.starts_with(|c: char| c.is_ascii_alphanumeric())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "version {v:?}: an image tag such as 17 or 8.4"
        )))
    }
}

impl DatabaseSource {
    pub fn version(&self) -> &str {
        if self.version.is_empty() {
            self.engine.default_version()
        } else {
            &self.version
        }
    }

    /// The image this database runs.
    pub fn image(&self) -> String {
        format!("docker:{}:{}", self.engine.repository(), self.version())
    }

    /// Fill the defaults in, so the stored record says what runs.
    pub fn normalize(&mut self, app: &str) {
        if self.version.is_empty() {
            self.version = self.engine.default_version().into();
        }
        if self.engine.has_database() {
            if self.database.is_none() {
                self.database = Some(app.replace('-', "_"));
            }
            if self.user.is_none() {
                self.user = Some(default_user(app));
            }
        }
    }

    pub fn database_name(&self, app: &str) -> String {
        self.database
            .clone()
            .unwrap_or_else(|| app.replace('-', "_"))
    }

    pub fn user_name(&self, app: &str) -> String {
        self.user.clone().unwrap_or_else(|| default_user(app))
    }

    pub fn validate(&self, app: &str) -> Result<()> {
        validate_version(self.version())?;
        if self.engine.has_database() {
            validate_ident("database", &self.database_name(app))?;
            validate_ident("user", &self.user_name(app))?;
            if self.engine == Engine::Postgres && self.user_name(app).starts_with("pg_") {
                return Err(Error::invalid(format!(
                    "user {:?}: Postgres reserves role names starting with pg_",
                    self.user_name(app)
                )));
            }
            if self.engine.has_root_password() && self.user_name(app) == "root" {
                return Err(Error::invalid(
                    "user: root is the engine's own administrator; pick another name",
                ));
            }
        }
        self.validate_urls(app)?;
        if !self.engine.has_database() && (self.database.is_some() || self.user.is_some()) {
            return Err(Error::invalid(format!(
                "{} has no database or user to set",
                self.engine
            )));
        }
        Ok(())
    }
}

/// The secret holding a database's password.
pub fn password_secret(app: &str) -> String {
    format!("db.{app}.password")
}

impl DatabaseSource {
    fn validate_urls(&self, app: &str) -> Result<()> {
        for (name, query) in &self.urls {
            crate::secrets::validate_name(name)?;
            if name.contains('/') || name.starts_with(&format!("db.{app}.")) {
                return Err(Error::invalid(format!(
                    "urls: {name:?} is not a secret isb can keep (db.{app}.* is the database's own, and a name with / is an external reference)"
                )));
            }
            let ok = query.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(
                        c,
                        '=' | '&' | '_' | '-' | '.' | '~' | '%' | '+' | ',' | ':' | '/'
                    )
            });
            if !ok || query.starts_with('?') {
                return Err(Error::invalid(format!(
                    "urls: {name:?}: {query:?} is a query string without its ?, such as sslmode=disable&connect_timeout=10"
                )));
            }
        }
        Ok(())
    }
}

/// The internal URL with `query` appended (`""` leaves it as it is).
pub fn with_query(url: &str, query: &str) -> String {
    match (query.is_empty(), url.contains('?')) {
        (true, _) => url.to_string(),
        (false, true) => format!("{url}&{query}"),
        (false, false) => format!("{url}?{query}"),
    }
}

/// The secret holding a MySQL or MariaDB root password.
pub fn root_password_secret(app: &str) -> String {
    format!("db.{app}.root-password")
}

/// The secret holding the internal connection URL (password included), for
/// apps: `DATABASE_URL=${{secret.db.<app>.url}}`.
pub fn url_secret(app: &str) -> String {
    format!("db.{app}.url")
}

/// The user a database gets when none is given: the app's name with `_`
/// for `-`, and `app_` in front of a name Postgres reserves (`pg_...`: an
/// app named `pg-main` would otherwise never initialize).
fn default_user(app: &str) -> String {
    let base = app.replace('-', "_");
    if base.starts_with("pg_") {
        format!("app_{base}")
    } else {
        base
    }
}

/// The secrets isb made for a database (and removes with it).
pub fn secrets(app: &str, engine: Engine) -> Vec<String> {
    let mut v = vec![password_secret(app), url_secret(app)];
    if engine.has_root_password() {
        v.push(root_password_secret(app));
    }
    v
}

/// The environment a database runs with, ahead of the app's own.
fn engine_env(app: &str, db: &DatabaseSource) -> Vec<(String, EnvValue)> {
    let s = |n: String| EnvValue::Secret { secret: n };
    let p = |v: &str| EnvValue::Plain(v.to_string());
    let (name, user) = (db.database_name(app), db.user_name(app));
    match db.engine {
        Engine::Postgres => vec![
            ("POSTGRES_DB".into(), p(&name)),
            ("POSTGRES_USER".into(), p(&user)),
            ("POSTGRES_PASSWORD".into(), s(password_secret(app))),
            // A subdirectory: the volume's root may hold lost+found.
            ("PGDATA".into(), p("/var/lib/postgresql/data/pgdata")),
        ],
        Engine::Mysql => vec![
            ("MYSQL_DATABASE".into(), p(&name)),
            ("MYSQL_USER".into(), p(&user)),
            ("MYSQL_PASSWORD".into(), s(password_secret(app))),
            ("MYSQL_ROOT_PASSWORD".into(), s(root_password_secret(app))),
        ],
        Engine::Mariadb => vec![
            ("MARIADB_DATABASE".into(), p(&name)),
            ("MARIADB_USER".into(), p(&user)),
            ("MARIADB_PASSWORD".into(), s(password_secret(app))),
            ("MARIADB_ROOT_PASSWORD".into(), s(root_password_secret(app))),
        ],
        Engine::Mongodb => vec![
            ("MONGO_INITDB_DATABASE".into(), p(&name)),
            ("MONGO_INITDB_ROOT_USERNAME".into(), p(&user)),
            ("MONGO_INITDB_ROOT_PASSWORD".into(), s(password_secret(app))),
        ],
        Engine::Redis => vec![
            ("REDIS_PASSWORD".into(), s(password_secret(app))),
            // redis-cli reads it, so health checks and dumps need no argv.
            ("REDISCLI_AUTH".into(), s(password_secret(app))),
        ],
    }
}

fn healthcheck(engine: Engine) -> Value {
    let test = match engine {
        Engine::Postgres => r#"pg_isready -q -h 127.0.0.1 -U "$POSTGRES_USER" -d "$POSTGRES_DB""#,
        Engine::Mysql => {
            r#"MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysqladmin ping -h 127.0.0.1 -uroot --silent"#
        }
        Engine::Mariadb => {
            r#"MYSQL_PWD="$MARIADB_ROOT_PASSWORD" mariadb-admin ping -h 127.0.0.1 -uroot --silent"#
        }
        Engine::Mongodb => {
            r#"mongosh --quiet --host 127.0.0.1 --eval "db.adminCommand('ping').ok""#
        }
        Engine::Redis => r#"redis-cli -h 127.0.0.1 --no-auth-warning ping | grep -q PONG"#,
    };
    json!({
        "test": ["CMD-SHELL", test],
        "interval": "5s",
        "timeout": "5s",
        "retries": 6,
        // First start initializes the data directory.
        "start_period": "120s",
    })
}

/// The spec a database app renders as: the engine's image, environment
/// (ahead of the app's own variables, which may add to it), data volume,
/// health check and, for Redis, command. One replica.
pub fn effective(spec: &AppSpec, db: &DatabaseSource) -> AppSpec {
    let mut s = spec.clone();
    let mut env = super::EnvFile::default();
    for (k, v) in engine_env(&spec.name, db) {
        env.set(&k, v);
    }
    for (k, v) in spec.env.vars() {
        env.set(k, v.clone());
    }
    s.env = env;
    s.volumes
        .insert(0, format!("{DATA_VOLUME}:{}", db.engine.data_path()));
    if s.healthcheck.is_none() {
        s.healthcheck = Some(healthcheck(db.engine));
    }
    if s.port.is_none() {
        s.port = Some(db.engine.port());
    }
    if db.engine == Engine::Redis && s.command.is_none() {
        s.command = Some(json!([
            "/bin/sh",
            "-c",
            r#"exec docker-entrypoint.sh redis-server --requirepass "$REDIS_PASSWORD" --save "60 1" --appendonly no"#
        ]));
    }
    s.replicas = s.replicas.min(1);
    s
}

/// Check what only databases restrict.
pub fn validate(spec: &AppSpec, db: &DatabaseSource) -> Result<()> {
    db.validate(&spec.name)?;
    if spec.build.is_some() {
        return Err(Error::invalid("a database is not built; drop `build`"));
    }
    if spec.replicas > 1 {
        return Err(Error::invalid(
            "a database runs one replica (two writers on one data volume corrupt it)",
        ));
    }
    for v in &spec.volumes {
        if v.split(':').next() == Some(DATA_VOLUME) {
            return Err(Error::invalid(format!(
                "volume name {DATA_VOLUME:?} is the database's own"
            )));
        }
    }
    Ok(())
}

/// How to reach a database, with the password as a secret reference. With
/// `password`, its value too.
pub fn connection(
    spec: &AppSpec,
    db: &DatabaseSource,
    org: &crate::org::OrgId,
    password: Option<&str>,
) -> Value {
    let stack = spec.stack().unwrap_or_default();
    let host = format!("{}.{stack}", spec.name);
    let fqdn = format!("{host}.{org}.isb");
    let port = db.engine.port();
    let (user, name) = if db.engine.has_database() {
        (
            Some(db.user_name(&spec.name)),
            Some(db.database_name(&spec.name)),
        )
    } else {
        (None, None)
    };
    let url = |pw: &str, host: &str, port: u16| -> String {
        let auth = match &user {
            Some(u) => format!("{u}:{pw}@"),
            None => format!("default:{pw}@"),
        };
        let mut u = format!("{}://{auth}{host}:{port}", db.engine.scheme());
        if let Some(n) = &name {
            u.push_str(&format!("/{n}"));
        }
        if db.engine == Engine::Mongodb {
            u.push_str("?authSource=admin");
        }
        u
    };
    let reference = format!("${{{{secret.{}}}}}", password_secret(&spec.name));
    // Published ports, as host:port pairs reachable from outside the org.
    let external: Vec<String> = spec
        .ports
        .iter()
        .filter_map(|p| {
            let parts: Vec<&str> = p.split(':').collect();
            match parts.as_slice() {
                [ip, host_port, _] => Some(format!("{ip}:{host_port}")),
                [host_port, _] => Some(format!("127.0.0.1:{host_port}")),
                _ => None,
            }
        })
        .collect();
    let mut v = json!({
        "engine": db.engine,
        "version": db.version(),
        "image": db.image(),
        "host": host,
        "fqdn": fqdn,
        "port": port,
        "password": {"secret": password_secret(&spec.name)},
        "url": url(&reference, &host, port),
        "url_secret": url_secret(&spec.name),
        "url_secrets": db.urls.keys().collect::<Vec<_>>(),
        "volume": format!("{stack}_{}_{DATA_VOLUME}", spec.name),
    });
    if let Some(u) = &user {
        v["user"] = json!(u);
    }
    if let Some(n) = &name {
        v["database"] = json!(n);
    }
    if db.engine.has_root_password() {
        v["root_password"] = json!({"secret": root_password_secret(&spec.name)});
    }
    if !external.is_empty() {
        v["external"] = json!(
            external
                .iter()
                .map(|hp| {
                    let (h, p) = hp.rsplit_once(':').unwrap_or((hp, ""));
                    url(&reference, h, p.parse().unwrap_or(port))
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(pw) = password {
        v["password_value"] = json!(pw);
        v["url_value"] = json!(url(pw, &host, port));
    }
    v
}

/// The internal URL with the real password, stored as [`url_secret`].
pub fn internal_url(spec: &AppSpec, db: &DatabaseSource, password: &str) -> String {
    connection(spec, db, &crate::org::OrgId::default_org(), Some(password))["url_value"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {

    #[test]
    fn postgres_users_never_start_with_pg() {
        let mut d: DatabaseSource =
            serde_json::from_value(serde_json::json!({"engine": "postgres"})).unwrap();
        assert_eq!(d.user_name("pg-main"), "app_pg_main");
        assert_eq!(
            d.database_name("pg-main"),
            "pg_main",
            "databases may start with pg_"
        );
        assert_eq!(d.user_name("pgcopy"), "pgcopy");
        assert!(d.validate("pg-main").is_ok());
        d.user = Some("pg_admin".into());
        assert!(d.validate("x").is_err());
    }
    use super::*;
    use crate::app::{Rendered, Source, render};

    fn db_spec(engine: &str) -> AppSpec {
        serde_json::from_value(json!({
            "name": "main-db", "project": "shop",
            "source": {"database": {"engine": engine}},
        }))
        .unwrap()
    }

    fn render_db(spec: &AppSpec) -> Rendered {
        let Source::Database(db) = &spec.source else {
            panic!()
        };
        let mut notes = vec![];
        render(spec, &db.image(), &mut notes).unwrap()
    }

    #[test]
    fn defaults_and_validation() {
        let mut s = db_spec("postgres");
        let Source::Database(db) = &mut s.source else {
            panic!()
        };
        db.normalize("main-db");
        assert_eq!(db.version, "17");
        assert_eq!(db.database.as_deref(), Some("main_db"));
        assert_eq!(db.user.as_deref(), Some("main_db"));
        assert_eq!(db.image(), "docker:postgres:17");
        s.validate().unwrap();
        let mut bad = s.clone();
        bad.replicas = 2;
        assert!(bad.validate().is_err(), "one replica");
        let mut bad = s.clone();
        bad.volumes = vec!["data:/x".into()];
        assert!(bad.validate().is_err(), "data is the database's");
        let mut bad = s.clone();
        if let Source::Database(d) = &mut bad.source {
            d.database = Some("x; drop".into());
        }
        assert!(bad.validate().is_err());
        let mut bad = s.clone();
        if let Source::Database(d) = &mut bad.source {
            d.version = "17 && rm".into();
        }
        assert!(bad.validate().is_err());
        let r = db_spec("redis");
        r.validate().unwrap();
        let mut bad = r.clone();
        if let Source::Database(d) = &mut bad.source {
            d.user = Some("u".into());
        }
        assert!(bad.validate().is_err(), "redis has no user");
        let m: AppSpec = serde_json::from_value(json!({
            "name": "m", "project": "p", "source": {"database": {"engine": "mysql", "user": "root"}},
        }))
        .unwrap();
        assert!(m.validate().is_err(), "root is mysql's own");
        assert_eq!(Engine::parse("mongo").unwrap(), Engine::Mongodb);
        assert!(Engine::parse("oracle").is_err());
        let e: Engine = serde_json::from_value(json!("mongo")).unwrap();
        assert_eq!(e, Engine::Mongodb);
    }

    /// A database's passwords change inside it before its replica gets
    /// them: `rotate` on exactly those secrets, and only on the database.
    #[test]
    fn passwords_rotate_inside_the_database() {
        for engine in ENGINES {
            let s = db_spec(engine.name());
            let r = render_db(&s);
            let rotating: Vec<&str> = r
                .secrets
                .iter()
                .filter(|(_, d)| d.rotate.is_some())
                .map(|(k, _)| k.as_str())
                .collect();
            let mut want = vec![format!("main-db.{}", password_secret("main-db"))];
            if engine.has_root_password() {
                want.push(format!("main-db.{}", root_password_secret("main-db")));
            }
            want.sort();
            assert_eq!(rotating, want, "{engine}");
            for root in [false, true] {
                let argv = engine.rotate_command(root);
                // The line parses as shell.
                let ok = std::process::Command::new("sh")
                    .args(["-n", "-c", &argv[2]])
                    .status()
                    .unwrap()
                    .success();
                assert!(ok, "{engine} root={root}: {}", argv[2]);
                // The new value only ever arrives on stdin.
                assert!(argv[2].contains("$(cat"), "{engine}");
            }
        }
    }

    #[test]
    fn credentials_render_as_secret_references() {
        for engine in ENGINES {
            let s = db_spec(engine.name());
            let r = render_db(&s);
            let svc = &r.service;
            assert_eq!(
                svc.image,
                format!(
                    "docker:{}:{}",
                    engine.repository(),
                    engine.default_version()
                )
            );
            // The password is a reference to the org secret, never a value.
            let pw = password_secret("main-db");
            assert!(
                svc.env
                    .secrets
                    .values()
                    .any(|k| k == &format!("main-db.{pw}")),
                "{engine}: {:?}",
                svc.env.secrets
            );
            assert_eq!(
                r.secrets[&format!("main-db.{pw}")].name.as_deref(),
                Some(pw.as_str())
            );
            assert!(r.secrets.values().all(|d| d.external));
            for v in svc.env.vars.values() {
                assert!(!v.contains("password"), "{engine}: {v}");
            }
            // Data on a volume; stop-first; one replica; a health check.
            assert_eq!(svc.volumes[0].source, "main-db_data");
            assert_eq!(svc.volumes[0].target, engine.data_path());
            assert!(r.volumes.contains_key("main-db_data"));
            let v = serde_json::to_value(svc).unwrap();
            assert_eq!(
                v["deploy"]["update_config"]["order"], "stop-first",
                "{engine}"
            );
            assert_eq!(svc.replicas(), 1);
            assert!(svc.healthcheck.is_some());
            assert!(svc.ports.is_empty(), "not published by default");
            if engine.has_root_password() {
                assert_eq!(svc.env.secrets.len(), 2, "{engine}");
            }
        }
        let pg = render_db(&db_spec("postgres"));
        assert_eq!(pg.service.env["POSTGRES_DB"], "main_db");
        assert_eq!(pg.service.env["POSTGRES_USER"], "main_db");
        assert_eq!(
            pg.service.env.secrets["POSTGRES_PASSWORD"],
            "main-db.db.main-db.password"
        );
        let redis = render_db(&db_spec("redis"));
        assert!(redis.service.command.is_some());
        assert_eq!(redis.service.env.secrets.len(), 2);
    }

    #[test]
    fn app_env_adds_to_the_engine_env() {
        let mut s = db_spec("postgres");
        s.env.set(
            "POSTGRES_INITDB_ARGS",
            EnvValue::Plain("--data-checksums".into()),
        );
        s.ports = vec!["127.0.0.1:15432:5432".into()];
        let r = render_db(&s);
        assert_eq!(r.service.env["POSTGRES_INITDB_ARGS"], "--data-checksums");
        assert_eq!(r.service.ports.len(), 1);
    }

    #[test]
    fn connection_names_the_secret() {
        let mut s = db_spec("postgres");
        s.ports = vec!["127.0.0.1:15432:5432".into()];
        let Source::Database(db) = &s.source else {
            panic!()
        };
        let org = crate::org::OrgId::new("acme").unwrap();
        let c = connection(&s, db, &org, None);
        assert_eq!(c["host"], "main-db.shop-production");
        assert_eq!(c["fqdn"], "main-db.shop-production.acme.isb");
        assert_eq!(c["port"], 5432);
        assert_eq!(c["password"]["secret"], "db.main-db.password");
        assert_eq!(
            c["url"],
            "postgres://main_db:${{secret.db.main-db.password}}@main-db.shop-production:5432/main_db"
        );
        assert_eq!(
            c["external"][0],
            "postgres://main_db:${{secret.db.main-db.password}}@127.0.0.1:15432/main_db"
        );
        assert!(c.get("password_value").is_none());
        let shown = connection(&s, db, &org, Some("pw1"));
        assert_eq!(shown["password_value"], "pw1");
        assert_eq!(
            internal_url(&s, db, "pw1"),
            "postgres://main_db:pw1@main-db.shop-production:5432/main_db"
        );
        let r = db_spec("redis");
        let Source::Database(rdb) = &r.source else {
            panic!()
        };
        assert_eq!(
            internal_url(&r, rdb, "x"),
            "redis://default:x@main-db.shop-production:6379"
        );
        let m = db_spec("mongodb");
        let Source::Database(mdb) = &m.source else {
            panic!()
        };
        assert!(internal_url(&m, mdb, "x").ends_with("/main_db?authSource=admin"));
    }

    #[test]
    fn dump_and_restore_take_credentials_from_the_instance() {
        for e in ENGINES {
            for argv in [e.dump_command(), e.restore_command()] {
                assert_eq!(argv.len(), 3);
                assert_eq!(argv[0], "/bin/sh");
                // Credentials are the instance's variables, expanded inside.
                if e != Engine::Redis {
                    assert!(argv[2].contains("PASSWORD\""), "{e}: {}", argv[2]);
                }
            }
        }
        assert!(Engine::Mysql.restores_from(Engine::Mariadb));
        assert!(!Engine::Postgres.restores_from(Engine::Mysql));
    }

    #[test]
    fn urls_take_a_query_string_and_never_the_databases_own_secrets() {
        assert_eq!(
            with_query("postgres://u:p@h:5432/d", ""),
            "postgres://u:p@h:5432/d"
        );
        assert_eq!(
            with_query(
                "postgres://u:p@h:5432/d",
                "sslmode=disable&connect_timeout=10"
            ),
            "postgres://u:p@h:5432/d?sslmode=disable&connect_timeout=10"
        );
        assert_eq!(
            with_query("mongodb://u:p@h:27017/d?authSource=admin", "tls=false"),
            "mongodb://u:p@h:27017/d?authSource=admin&tls=false"
        );
        let db = |urls: Value| -> DatabaseSource {
            serde_json::from_value(json!({"engine": "postgres", "urls": urls})).unwrap()
        };
        assert!(
            db(json!({"dsn.main-db.web": "sslmode=disable", "dsn.x": ""}))
                .validate("main-db")
                .is_ok()
        );
        for bad in [
            json!({"db.main-db.url": ""}),
            json!({"db.main-db.password": ""}),
            json!({"op/vault/item": ""}),
            json!({"dsn.x": "?sslmode=disable"}),
            json!({"dsn.x": "a=b c"}),
            json!({"dsn.x": "a=b#frag"}),
        ] {
            assert!(db(bad.clone()).validate("main-db").is_err(), "{bad}");
        }
    }
}
