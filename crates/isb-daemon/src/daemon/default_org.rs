//! The default org at startup: `isb-default`, created when missing, and
//! service names for every org once the host can have them.

use super::*;

/// Say so at startup when incus is too old for OCI images (`docker:` apps).
pub(super) fn warn_old_incus(client: &Client) {
    if let Some(m) = client.oci_unsupported() {
        eprintln!("isb serve: WARNING: {m}");
    }
}

/// Create the default org (`isb-default`) if it is missing, and warn about
/// default-org stacks whose instances are still in incus' own `default`
/// project, which is no org: the controller recreates them in
/// `isb-default` with new volumes and leaves the old instances where they
/// are.
pub(super) fn ensure(client: &Client, store: &Store) {
    match crate::org::ensure_default(client, &mut |l| eprintln!("isb serve: default org: {l}")) {
        Ok(true) => eprintln!(
            "isb serve: created the default org (incus project {})",
            crate::org::DEFAULT_ORG_PROJECT
        ),
        Ok(false) => {}
        Err(e) => eprintln!("isb serve: WARNING: the default org: {e}"),
    }
    watch_service_names(client);
    let stacks: Vec<String> = store
        .load_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.org.is_default())
        .map(|d| d.name)
        .collect();
    let stray = stray_default_stacks(client, &stacks);
    if !stray.is_empty() {
        eprintln!(
            "isb serve: WARNING: default-org stack(s) {} have instances in incus' default project, which is not an org: they run afresh in {} (new volumes), and the old instances and volumes there are left for you to move or delete",
            stray.join(", "),
            crate::org::DEFAULT_ORG_PROJECT
        );
    }
}

/// Turn service names on for every org that lacks them, now and then once a
/// minute: `isb host setup` may come after `isb serve install`, or an org be
/// made on a host that had no directory yet, and neither may need a restart.
/// An org that has them costs two reads a minute.
fn watch_service_names(client: &Client) {
    let client = client.clone();
    let spawned = std::thread::Builder::new()
        .name("service-names".into())
        .spawn(move || {
            // What each pass has to say is said once: a warning that
            // stays true would repeat every minute.
            let mut said = std::collections::BTreeSet::new();
            loop {
                let mut lines = Vec::new();
                let _ = crate::org::ensure_all_service_names(&client, &mut |l| {
                    lines.push(l.to_string());
                });
                for l in lines {
                    if said.insert(l.clone()) {
                        eprintln!("isb serve: {l}");
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        });
    if let Err(e) = spawned {
        eprintln!("isb serve: WARNING: cannot watch service names: {e}");
    }
}

/// Which of `stacks` have instances in incus' `default` project.
fn stray_default_stacks(client: &Client, stacks: &[String]) -> Vec<String> {
    if stacks.is_empty() {
        return Vec::new();
    }
    let key = format!("user.{}", crate::stack::LABEL_STACK);
    let Ok(v) = client
        .clone()
        .project("default")
        .get("/1.0/instances?recursion=1")
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["config"][&key].as_str())
        .filter(|s| stacks.iter().any(|n| n == s))
        .map(String::from)
        .collect();
    out.sort();
    out.dedup();
    out
}
