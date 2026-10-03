//! Builds: turn a source tree into an OCI image in the org's registry.
//!
//! Every build runs in a fresh isb sandbox in the org (a VM when the source
//! is untrusted), never on the host. The result is an image reference incus
//! can pull (see TASKS.md, "Local registry").

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::org::OrgId;

/// How a source tree becomes an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Builder {
    /// Railpack detects the language and builds without a Dockerfile.
    Railpack,
    Nixpacks,
    /// A Dockerfile, relative to the context.
    Dockerfile {
        #[serde(default = "default_dockerfile")]
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// Cloud Native Buildpacks with the given builder image.
    Buildpacks {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        builder: Option<String>,
    },
}

fn default_dockerfile() -> String {
    "Dockerfile".into()
}

/// One build.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub org: OrgId,
    /// The app the image belongs to: names the repository in the registry
    /// (`<org>/<app>`) and the build cache volume.
    pub app: String,
    /// A checked-out source tree on the host. The build reads it, never
    /// writes it.
    pub context: PathBuf,
    /// A subdirectory of `context` to build from.
    pub subdir: Option<String>,
    pub builder: Builder,
    /// Build-time variables (Dockerfile `ARG`s, buildpack env).
    pub args: Vec<(String, String)>,
    /// The tag to push, e.g. the commit SHA.
    pub tag: String,
    /// Build in a VM rather than a container.
    pub untrusted: bool,
}

/// What a build produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltImage {
    /// What a compose `image:` takes to run it, e.g.
    /// `registry:<org>/<app>:<tag>` in the form the stack layer resolves.
    pub image: String,
    /// The manifest digest (`sha256:...`), for rollbacks that must not
    /// follow a moved tag.
    pub digest: String,
}

/// Run a build with `base` (an unscoped client; the build's sandbox lives
/// in the request's org), streaming its log lines to `log`.
pub fn run(_base: &Client, _req: &BuildRequest, _log: &mut dyn FnMut(&str)) -> Result<BuiltImage> {
    Err(Error::invalid("builds are not implemented yet"))
}
