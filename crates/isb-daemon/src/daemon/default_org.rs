//! The default org at startup: `isb-default`, created when missing.

use super::*;

/// Create the default org (`isb-default`) if it is missing, and warn about
/// default-org stacks whose instances are still in incus' own `default`
/// project, which is no org: the controller looks for them in
/// `isb-default` and will not find them there.
pub(super) fn ensure(client: &Client, store: &Store) {
    match crate::org::ensure_default(client, &mut |l| eprintln!("isb serve: default org: {l}")) {
        Ok(true) => eprintln!(
            "isb serve: created the default org (incus project {})",
            crate::org::DEFAULT_ORG_PROJECT
        ),
        Ok(false) => {}
        Err(e) => eprintln!("isb serve: WARNING: the default org: {e}"),
    }
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
            "isb serve: WARNING: default-org stack(s) {} have instances in incus' default project, which is not an org; the default org is {} (remove and redeploy them)",
            stray.join(", "),
            crate::org::DEFAULT_ORG_PROJECT
        );
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
