//! What an OCI instance needs beyond config: its command line, and work
//! done on it between creation and first start.

use crate::error::Result;

/// Quote argv for `oci.entrypoint`, which incus splits on whitespace with
/// quotes grouping. There is no escape character, so an argument may not
/// contain both kinds of quote.
pub fn oci_command_line(argv: &[String]) -> std::result::Result<String, String> {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
                Ok(a.clone())
            } else if !a.contains('"') {
                Ok(format!("\"{a}\""))
            } else if !a.contains('\'') {
                Ok(format!("'{a}'"))
            } else {
                Err(format!(
                    "argument {a:?} has both ' and \" in it, which an OCI command line cannot carry; use a script"
                ))
            }
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(|v| v.join(" "))
}

/// See [`Desired::before_start`]: called with the instance's name.
#[derive(Clone)]
pub struct BeforeStart(pub std::sync::Arc<BeforeStartFn>);

/// The work: given a client on the instance's project, and its name.
pub type BeforeStartFn = dyn Fn(&crate::client::Client, &str) -> Result<()> + Send + Sync;

impl std::fmt::Debug for BeforeStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BeforeStart")
    }
}
