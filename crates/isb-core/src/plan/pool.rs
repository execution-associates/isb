//! Which storage pool an instance's root disk and volumes go on.

use super::HostFacts;
use crate::error::{Error, Result};

/// The pool new plain sandboxes go on when it exists. Make it copy-on-write
/// (zfs or btrfs), so each sandbox shares its image instead of copying it.
/// Stacks, apps, builds and databases never pick it on their own.
pub const SANDBOX_POOL: &str = "isb-sandboxes";

impl HostFacts {
    /// `auto`: [`HostFacts::prefer`] when it exists, then `incus-zfs`, else
    /// `default`, else the first pool.
    pub fn pick_pool(&self, requested: Option<&str>) -> Result<String> {
        match requested {
            Some(p) if p != "auto" && !p.is_empty() => {
                if self.pools.is_empty() || self.pools.iter().any(|x| x == p) {
                    Ok(p.to_string())
                } else {
                    Err(Error::invalid(format!(
                        "storage pool {p:?} does not exist (have: {})",
                        self.pools.join(", ")
                    )))
                }
            }
            _ => self
                .prefer
                .iter()
                .map(String::as_str)
                .chain(["incus-zfs", "default"])
                .find(|c| self.pools.iter().any(|p| p == c))
                .map(str::to_string)
                .or_else(|| self.pools.first().cloned())
                .ok_or_else(|| Error::invalid("no storage pools exist")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(pools: &[&str], prefer: Option<&str>) -> HostFacts {
        HostFacts {
            pools: pools.iter().map(|p| p.to_string()).collect(),
            prefer: prefer.map(str::to_string),
            ..HostFacts::default()
        }
    }

    #[test]
    fn a_plain_sandbox_prefers_the_sandbox_pool_only_when_it_exists() {
        let h = host(&["default", SANDBOX_POOL], Some(SANDBOX_POOL));
        assert_eq!(h.pick_pool(None).unwrap(), SANDBOX_POOL);
        assert_eq!(h.pick_pool(Some("auto")).unwrap(), SANDBOX_POOL);
        // An explicit pool always wins.
        assert_eq!(h.pick_pool(Some("default")).unwrap(), "default");
        let h = host(&["default", "incus-zfs"], Some(SANDBOX_POOL));
        assert_eq!(h.pick_pool(None).unwrap(), "incus-zfs");
    }

    #[test]
    fn everything_else_never_picks_the_sandbox_pool() {
        let h = host(&[SANDBOX_POOL, "default"], None);
        assert_eq!(h.pick_pool(None).unwrap(), "default");
    }
}
