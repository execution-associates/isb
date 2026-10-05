//! `FixOwner`: chown a mount point (and the root-owned directories above it,
//! up to the user's home) to the user that will use it.

use std::time::Duration;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::{self, Stdin};

const OWNER_SCRIPT: &str = r#"set -e
owner="$1"; path="$2"
user="${owner%%:*}"
group=""
case "$owner" in *:*) group="${owner#*:}" ;; esac
home=""
if ent="$(getent passwd "$user")"; then
  uid="$(printf %s "$ent" | cut -d: -f3)"
  gid="$(printf %s "$ent" | cut -d: -f4)"
  home="$(printf %s "$ent" | cut -d: -f6)"
else
  case "$user" in ''|*[!0-9]*) echo "isb: no such user: $user" >&2; exit 1 ;; esac
  uid="$user"; gid="$user"
fi
[ -n "$group" ] || group="$gid"
chown "$uid:$group" "$path"
# Parents the mount conjured are root-owned; fix those inside the user's home
# only, and stop at the first one that is not root's.
[ -n "$home" ] && [ "$home" != / ] || exit 0
case "$path" in
  "$home"/*)
    d="$(dirname "$path")"
    while [ "$d" != "$home" ] && [ "$d" != "/" ]; do
      [ "$(stat -c %u "$d")" = 0 ] || break
      chown "$uid:$group" "$d"
      d="$(dirname "$d")"
    done ;;
esac
"#;

pub(super) fn fix_owner(client: &Client, name: &str, path: &str, owner: &str) -> Result<()> {
    let argv: Vec<String> = ["sh", "-c", OWNER_SCRIPT, "isb-owner", owner, path]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = exec::run_captured(
        client,
        name,
        &argv,
        &exec::Request::default(),
        Stdin::Null,
        Some(Duration::from_secs(60)),
    )?;
    if !out.success() {
        return Err(Error::OperationFailed {
            step: format!("chown {owner} {path} in {name}"),
            message: out.stderr_text().trim().to_string(),
        });
    }
    Ok(())
}
