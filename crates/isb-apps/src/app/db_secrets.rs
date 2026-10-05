//! A database app's credentials as org secrets: its passwords, generated
//! once, and the URL secrets that carry the password, kept in step with it.

use super::Apps;
use crate::app::{AppSpec, Source, git};
use crate::error::{Error, Result};
use crate::org::OrgId;

impl Apps {
    /// A database's passwords, generated unless they exist (a database
    /// re-created over its kept volume needs the passwords that volume was
    /// initialized with), and its connection URL.
    pub(super) fn database_credentials(
        &self,
        org: &OrgId,
        spec: &AppSpec,
        db: &crate::app::DatabaseSource,
    ) -> Result<()> {
        use crate::app::database as d;
        let mut names = vec![d::password_secret(&spec.name)];
        if db.engine.has_root_password() {
            names.push(d::root_password_secret(&spec.name));
        }
        for n in &names {
            match self.inner.secrets.inspect(org, n) {
                Ok(_) => {}
                Err(e) if e.is_not_found() => {
                    self.inner
                        .secrets
                        .set(org, n, git::random_hex(16).as_bytes())?;
                }
                Err(e) => return Err(e),
            }
        }
        self.write_urls(org, spec, db)?;
        Ok(())
    }

    /// Store the URL secret and the database's `urls` again from its
    /// current password; returns the names whose value moved.
    pub(super) fn write_urls(
        &self,
        org: &OrgId,
        spec: &AppSpec,
        db: &crate::app::DatabaseSource,
    ) -> Result<Vec<String>> {
        use crate::app::database as d;
        let (pw, _) = self
            .inner
            .secrets
            .get(org, &d::password_secret(&spec.name))?;
        let pw = String::from_utf8(pw).map_err(|_| Error::invalid("the password is not text"))?;
        let url = d::internal_url(spec, db, pw.trim());
        let wanted = std::iter::once((d::url_secret(&spec.name), url.clone())).chain(
            db.urls
                .iter()
                .map(|(n, q)| (n.clone(), d::with_query(&url, q))),
        );
        let mut moved = Vec::new();
        for (name, value) in wanted {
            let before = self.inner.secrets.version(org, &name).ok();
            let m = self.inner.secrets.put(org, &name, value.as_bytes())?;
            if Some(m.version) != before {
                moved.push(m.name);
            }
        }
        Ok(moved)
    }

    /// The secret `name` got a new value: when it is a database app's
    /// password (`db.<app>.password`), store the URL secret and the
    /// database's `urls` again with it. Returns the secrets whose value
    /// moved.
    pub fn database_password_changed(&self, org: &OrgId, name: &str) -> Result<Vec<String>> {
        let Some(app) = name
            .strip_prefix("db.")
            .and_then(|r| r.strip_suffix(".password"))
        else {
            return Ok(vec![]);
        };
        let a = match self.get(org, app) {
            Ok(a) => a,
            Err(e) if e.is_not_found() => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let Source::Database(db) = &a.spec.source else {
            return Ok(vec![]);
        };
        self.write_urls(org, &a.spec, db)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::secrets::{Keyring, LocalDriver, Secrets};

    #[test]
    fn url_secrets_follow_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let k = Keyring::new(age::x25519::Identity::generate(), vec![]);
        let secrets = Arc::new(Secrets::new(LocalDriver::new(dir.path(), Arc::new(k))));
        let client = crate::client::Client::with_socket("/nonexistent/isb-test/incus.sock");
        let store = crate::stack::Store::open(dir.path()).unwrap();
        let ctl = crate::stack::Controller::start(
            client.clone(),
            store,
            Duration::from_secs(60),
            secrets.clone(),
        )
        .unwrap();
        let ap = Apps::new(dir.path(), client, ctl, secrets.clone());
        let org = OrgId::default_org();
        let spec: AppSpec = serde_json::from_value(serde_json::json!({
            "name": "main-db", "project": "shop",
            "source": {"database": {"engine": "postgres", "urls": {
                "dsn.main-db.web": "sslmode=disable", "dsn.main-db.plain": "",
            }}},
        }))
        .unwrap();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        ap.create(&org, spec).unwrap();
        let Source::Database(db) = &ap.get(&org, "main-db").unwrap().spec.source else {
            panic!("a database")
        };
        let value = |n: &str| String::from_utf8(secrets.get(&org, n).unwrap().0).unwrap();
        ap.database_credentials(&org, &ap.get(&org, "main-db").unwrap().spec, db)
            .unwrap();
        let url = value("db.main-db.url");
        assert_eq!(value("dsn.main-db.web"), format!("{url}?sslmode=disable"));
        assert_eq!(value("dsn.main-db.plain"), url);

        secrets
            .set(&org, "db.main-db.password", b"n3w-pass")
            .unwrap();
        let mut moved = ap
            .database_password_changed(&org, "db.main-db.password")
            .unwrap();
        moved.sort();
        assert_eq!(
            moved,
            ["db.main-db.url", "dsn.main-db.plain", "dsn.main-db.web"]
        );
        assert!(value("dsn.main-db.web").contains(":n3w-pass@"));
        assert!(value("dsn.main-db.web").ends_with("?sslmode=disable"));
        assert!(
            ap.database_password_changed(&org, "db.main-db.password")
                .unwrap()
                .is_empty(),
            "nothing moves twice"
        );
        assert!(
            ap.database_password_changed(&org, "dsn.main-db.web")
                .unwrap()
                .is_empty()
        );

        // An entry added later is written by the next deploy (which then
        // fails here, with no incusd: the secrets come first).
        ap.update(
            &org,
            "main-db",
            &serde_json::json!({"source": {"database": {"urls": {"dsn.main-db.jobs": "x=1"}}}}),
        )
        .unwrap();
        assert!(secrets.get(&org, "dsn.main-db.jobs").is_err());
        let d = ap
            .deploy(&org, "main-db", super::super::Trigger::Api, "t", None)
            .unwrap();
        ap.wait(&org, "main-db", d.id, Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            value("dsn.main-db.jobs"),
            format!("{}?x=1", value("db.main-db.url"))
        );
    }
}
