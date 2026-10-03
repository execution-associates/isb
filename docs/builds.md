# Builds and the local registry

isb turns a source directory into an OCI image in a fresh sandbox, pushes it
to a registry on the host, and runs it from there as a stack service:

```sh
isb registry setup                 # once per host
sudo isb host setup                # once: incus trusts the registry
isb --org acme build ./web --app web --tag v1
#   ... build log ...
#   registry:web:v1@sha256:5c1f...
```

```yaml
# compose.yaml
services:
  web:
    image: registry:web:v1
    ports: ["127.0.0.1:8080:3000"]
```

```sh
isb --org acme stack deploy -f compose.yaml web
```

## Images in a compose file: `registry:`

```yaml
image: registry:APP:TAG                # the tag as it is now (default tag: latest)
image: registry:APP@sha256:DIGEST      # exactly this image
image: registry:APP:TAG@sha256:DIGEST  # the same; the tag is only for people
```

`APP` is the app's repository **in the org the stack or sandbox lives in**:
`registry:web:v1` in org `acme` is `acme/web:v1` in the registry, and nothing
else. There is no way to write another org's repository: a `/` in `APP` is an
error, and the registry's own address (`oci:127.0.0.1:5480/...`, or any
loopback registry) is refused as an `oci:` image. A digest that does not exist
in the org's repository fails the deploy.

When a stack is deployed, every `registry:` tag is resolved to the digest it
names at that moment, and that digest is part of the service's revision
(`isb stack config` shows the file as written; the stored definition keeps the
digests). So:

- deploying the same file after the tag moved (a rebuild of `v1`) is an
  `update`, and an unchanged tag is `unchanged`;
- `isb stack redeploy NAME SERVICE` resolves the tag again;
- `isb stack rollback NAME` runs exactly the digests the previous deployment
  ran, wherever the tags have gone since.

An OCI image from the registry runs like any other OCI image (`command`,
`entrypoint`, numeric `user`; see [spec.md](spec.md#image)).

## Building

```text
isb [--org ORG] build DIR --app APP [--tag TAG] [--builder railpack|nixpacks|dockerfile]
                          [--dockerfile PATH] [--target STAGE] [--arg K=V]...
                          [--subdir DIR] [--untrusted] [--timeout 30m] [-d]
```

The build runs in the daemon (`isb serve`), which alone pushes to the
registry; the CLI streams its log and prints the image to put in a compose
file. `-d` prints the build id instead (follow it with the `build_logs`
tool). Agents use the same tools: `build_run`, `build_logs`, `build_list`.

| Builder | What it does |
|---|---|
| `railpack` (default) | Detects the language (Node, Python, Go, Ruby, PHP, Rust, Java, static sites, ...) and builds with railpack's BuildKit frontend. `--arg`s are build environment (secrets to the build, not baked into the image). |
| `dockerfile` | BuildKit's Dockerfile frontend: `--dockerfile` (default `Dockerfile`, relative to the directory), `--target`, `--arg` as `ARG`s. Chosen by `--dockerfile` alone. |
| `nixpacks` | Nixpacks generates a Dockerfile, BuildKit builds it. `--arg`s are environment. |
| buildpacks | **Not supported yet.** `pack` drives a docker daemon to run the buildpack lifecycle; running one inside the build sandbox would need nesting. Railpack covers the same languages. |

From code (the app layer), the call is the library's:

```rust
let built = isb::build::run(&client, &isb::build::BuildRequest {
    org, app: "web".into(), context: checkout_dir, subdir: None,
    builder: isb::build::Builder::Railpack, args: vec![],
    tag: commit_sha, untrusted: false,
}, &mut |line| println!("{line}"))?;
// built.image: "registry:web:<sha>@sha256:...", built.digest: "sha256:..."
```

`isb::build::run_with` takes [`BuildOptions`](#limits) as well.

### Where a build runs

Every build gets a **fresh sandbox in the org's own incus project**, deleted
when the build ends, however it ends (success, failure, timeout):

- **A container** by default: unprivileged, with its own uid range, on the
  org's bridge behind the org's ACL, counted against the org's quota. BuildKit
  runs as root inside it and gives each `RUN` step its own namespaces and an
  overlayfs snapshot; incus allows that in an unprivileged container without
  `security.nesting`, which the org project keeps blocked.
- **A VM** with `--untrusted` (`untrusted: true`): the build gets its own
  kernel, so a kernel exploit in a build step stays in the VM. The org project
  allows VMs; the host needs KVM. A VM costs about 40 s more (boot).

The source directory is copied in as a tar (symlinks stay links, never
followed on the host; at most 2 GiB); the host directory is never mounted.
The build writes the image as an OCI layout inside the sandbox; the daemon
copies it out over the incus API and pushes it. A build sandbox has no
credentials for the registry and no route to it.

### Build cache

BuildKit keeps its state (layers and `--mount=type=cache` directories) on a
per-app volume in the org: `build-cache-<app>` (a filesystem volume mounted at
`/var/lib/buildkit`) for container builds, `build-cache-<app>-vm` (a 20 GiB
block volume, formatted on first use) for VM builds. BuildKit's own garbage
collection keeps each under 10 GB. Measured on titan (container builds,
nothing changed between the two runs):

| App | First build | Rebuild |
|---|---|---|
| Node (railpack, one dependency) | 61 s | 14 s |
| Python (railpack, Flask) | 65 s | 25 s |
| Dockerfile (busybox) | 37 s (with the one-time builder image) | 9 s |
| Dockerfile in a VM | 160 s (with the one-time VM builder image) | 68 s |

`isb volume rm build-cache-<app>` (in the org) forgets an app's cache.

### The builder image

The tools come from one image, `isb-builder/<hash>` (`isb-builder-vm/<hash>`
for VMs), made the first time a build needs it from the recipe in
`src/build/builder-image.sh` (Ubuntu 24.04 plus the tools below, each download
checked against a pinned SHA-256). It is prepared in the `isb-system` project,
never in an org (an org could otherwise tamper with the image every org builds
with), and shared by every org through the host's images. A changed recipe is
a new hash, so the next build prepares a new image; old ones can be deleted
with `incus image delete`.

| Tool | Version |
|---|---|
| BuildKit (buildkitd, buildctl, runc) | v0.33.1 |
| railpack | v0.40.1, frontend `ghcr.io/railwayapp/railpack-frontend:v0.40.1` (pinned by digest) |
| nixpacks | v1.41.0 |

### Limits

| Setting | Default | Environment of `isb serve` |
|---|---|---|
| Whole build (sandbox to push) | 30 minutes | `ISB_BUILD_TIMEOUT`, or `--timeout` per build |
| Build sandbox CPUs / memory | 2 / 4GiB (counted against the org's quota) | `ISB_BUILD_CPUS`, `ISB_BUILD_MEMORY` |
| VM cache disk | 20GiB | `ISB_BUILD_CACHE_SIZE` |

A timed-out build is killed and its sandbox deleted. The image is staged in
`<state>/builds/` between the sandbox and the registry, and removed after the
push.

## The registry

One per host, run by isb:

- an OCI container (`registry:2.8.3`, pinned by digest) named `registry` in
  the incus project `isb-system`, which is not an org (`system` is a reserved
  org name); its blobs on the volume `registry-data`;
- **no network interface**: it listens on a unix socket inside its container,
  and a proxy device listening on the host's `127.0.0.1:5480` is its only way
  in. incusd and the daemon reach it; no org network can;
- **TLS** with a certificate from an isb CA. The CA's key and the
  certificate's key live in `<state>/registry/` (0600) under the daemon's state
  directory; the CA certificate is also recorded on the `isb-system` project,
  where any isb process finds it with the registry's address.

```text
isb registry setup [--port 5480] [--renew] [--state-dir DIR]
isb [--org ORG] registry ls [--json]
isb registry gc [--keep 10] [--dry-run]       platform admins
```

`isb registry setup` creates or reconciles all of it (safe to repeat; restart
`isb serve` afterwards so it pushes there). incus pulls OCI images only over
https, through skopeo, which trusts a registry's CA at
`/etc/containers/certs.d/<host:port>/ca.crt`; `sudo isb host setup` writes that
file ([orgs.md](orgs.md#host-firewall-isb-host-setup)). To undo it all: `incus
project delete isb-system` after deleting its instance and volume, and remove
`/etc/containers/certs.d/127.0.0.1:5480`.

### Retention

`isb registry gc` (the `registry_gc` tool) keeps, per repository, the newest
`--keep` tags (by push time) and every image a deployed stack runs **or would
roll back to**, and deletes the other manifests. A deployed digest whose tag
has moved on gets a tag of retention's own (`isb-keep-<digest>`), so the
registry's collection of untagged manifests spares it; the tag goes once no
deployment uses the digest. Then the registry's `garbage-collect
--delete-untagged` frees the unreferenced blobs (pushes wait meanwhile) and
the registry restarts. `--dry-run` lists what would go. Preview images
(`pr-<n>-<sha>`, [previews.md](previews.md)) never count among the newest:
they are kept while deployed and deleted otherwise.

### What it does not do (yet)

- No authentication: anything on the host that can open `127.0.0.1:5480` can
  read and write every org's images. Org sandboxes cannot (no route, and
  `bind: guest` ports are refused to remote callers and to org projects), but
  host users and the default project's trusted local callers can.
- No external registries as push targets, no image signing, no
  vulnerability scanning.
