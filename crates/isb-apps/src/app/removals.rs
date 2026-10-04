//! What replacing an app's settings with a document takes away.

use serde_json::Value;

use super::AppSpec;

/// What replacing `old` with `new` takes away: domains, env vars, volumes,
/// ports and files the new settings no longer have, and a previews config,
/// healthcheck, resources, command, port, user or working directory that
/// goes back to its default. A changed entry (a domain's flags, a port's
/// host side) is not a removal; additions never are. One line each, such
/// as `domains: app.example.com`.
pub fn removals(old: &AppSpec, new: &AppSpec) -> Vec<String> {
    let mut out = Vec::new();
    let gone = |out: &mut Vec<String>, what: &str, o: Vec<String>, n: Vec<String>| {
        for k in o.into_iter().filter(|k| !n.contains(k)) {
            out.push(format!("{what}: {k}"));
        }
    };
    let host = |d: &serde_json::Map<String, Value>| {
        let h = d.get("host").and_then(Value::as_str).unwrap_or("?");
        format!("{h}{}", d.get("path").and_then(Value::as_str).unwrap_or(""))
    };
    gone(
        &mut out,
        "domains",
        old.domains.iter().map(host).collect(),
        new.domains.iter().map(host).collect(),
    );
    let keys = |a: &AppSpec| a.env.vars().map(|(k, _)| k.to_string()).collect();
    gone(&mut out, "env", keys(old), keys(new));
    // A volume is its mount path, a published port its container side.
    let mount = |v: &String| v.split(':').nth(1).unwrap_or(v).to_string();
    gone(
        &mut out,
        "volumes",
        old.volumes.iter().map(mount).collect(),
        new.volumes.iter().map(mount).collect(),
    );
    let target = |p: &String| {
        let t = p.rsplit(':').next().unwrap_or(p);
        t.split('/').next().unwrap_or(t).to_string()
    };
    gone(
        &mut out,
        "ports",
        old.ports.iter().map(target).collect(),
        new.ports.iter().map(target).collect(),
    );
    gone(
        &mut out,
        "files",
        old.files.iter().map(|f| f.path.clone()).collect(),
        new.files.iter().map(|f| f.path.clone()).collect(),
    );
    let reset = |out: &mut Vec<String>, what: &str, was: bool, is: bool| {
        if was && !is {
            out.push(format!("{what}: reset to the default"));
        }
    };
    reset(&mut out, "port", old.port.is_some(), new.port.is_some());
    reset(
        &mut out,
        "healthcheck",
        old.healthcheck.is_some(),
        new.healthcheck.is_some(),
    );
    reset(
        &mut out,
        "resources",
        old.resources.is_some(),
        new.resources.is_some(),
    );
    reset(
        &mut out,
        "command",
        old.command.is_some(),
        new.command.is_some(),
    );
    reset(
        &mut out,
        "previews",
        old.previews.is_some(),
        new.previews.is_some(),
    );
    reset(&mut out, "user", old.user.is_some(), new.user.is_some());
    reset(
        &mut out,
        "working_dir",
        old.working_dir.is_some(),
        new.working_dir.is_some(),
    );
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::manifest::parse;

    fn spec(extra: &str) -> String {
        format!("name: web\nproject: shop\nsource:\n  image: docker:nginx:1.27\n{extra}")
    }

    fn parsed(extra: &str) -> AppSpec {
        parse(&spec(extra)).unwrap()
    }

    const FULL: &str = "env: |\n  A=1\n  B=2\ndomains:\n  - host: a.example.com\n    https: true\n  - host: b.example.com\nvolumes: [\"data:/data\"]\nports: [\"127.0.0.1:8080:80\"]\nport: 80\nreplicas: 2\nfiles:\n  - {path: /etc/app.conf, secret: conf}\nhealthcheck: {test: [\"CMD\", \"true\"]}\ncommand: [\"run\"]\nuser: \"1000\"\n";

    #[test]
    fn removals_name_what_a_short_document_drops() {
        let old = parsed(FULL);
        // Nothing left out: nothing removed. Changes and additions are not removals.
        assert!(removals(&old, &old).is_empty());
        let mut new = old.clone();
        new.domains[0].insert("https".into(), json!(false));
        new.domains
            .push(serde_json::from_value(json!({"host": "c.example.com"})).unwrap());
        new.ports = vec!["0.0.0.0:9090:80".into(), "443".into()];
        new.volumes.push("cache:/cache".into());
        new.replicas = 5;
        new.env
            .set("C", crate::app::env::EnvValue::Plain("3".into()));
        assert!(
            removals(&old, &new).is_empty(),
            "{:?}",
            removals(&old, &new)
        );

        let r = removals(&old, &parsed(""));
        assert_eq!(
            r,
            [
                "domains: a.example.com",
                "domains: b.example.com",
                "env: A",
                "env: B",
                "volumes: /data",
                "ports: 80",
                "files: /etc/app.conf",
                "port: reset to the default",
                "healthcheck: reset to the default",
                "command: reset to the default",
                "user: reset to the default",
            ]
        );
        // One of two domains.
        let one = parsed("domains:\n  - host: a.example.com\n");
        let r = removals(&old, &one);
        assert!(r.contains(&"domains: b.example.com".to_string()));
        assert!(!r.iter().any(|x| x.contains("a.example.com")));
    }
}
