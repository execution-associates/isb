//! The builder API: `SandboxSpec::new(..).cpus(..)`, `Volume::bind(..)`.

use super::*;

/// Mount builders: `Volume::bind(host)`, `Volume::named(name)`.
pub struct Volume;

impl Volume {
    /// Bind-mount a host path. The target is set by [`SandboxSpec::volume`].
    pub fn bind(host_path: impl Into<String>) -> VolumeSpec {
        VolumeSpec {
            mount_type: MountType::Bind,
            source: host_path.into(),
            ..Default::default()
        }
    }

    /// Mount a named custom volume (created if missing).
    pub fn named(name: impl Into<String>) -> VolumeSpec {
        VolumeSpec {
            mount_type: MountType::Volume,
            source: name.into(),
            ..Default::default()
        }
    }
}

impl VolumeSpec {
    /// Named volumes: the volume must already exist; isb never creates it.
    pub fn external(mut self, external: bool) -> Self {
        self.external = external;
        self
    }
    pub fn read_only(mut self, ro: bool) -> Self {
        self.read_only = ro;
        self
    }
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }
    /// The mount point's octal mode (`"0770"`).
    pub fn mode(mut self, mode: impl Into<String>) -> Self {
        self.mode = Some(mode.into());
        self
    }
    pub fn device(mut self, name: impl Into<String>) -> Self {
        self.device = Some(name.into());
        self
    }
    pub fn pool(mut self, pool: impl Into<String>) -> Self {
        self.pool = Some(pool.into());
        self
    }
    /// Named volumes: do not seed an empty volume from the image.
    pub fn nocopy(mut self, nocopy: bool) -> Self {
        self.volume.nocopy = nocopy;
        self
    }
    pub fn option(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.options.insert(k.into(), v.into());
        self
    }
}

/// Port binding builders.
pub struct PortBinding;

impl PortBinding {
    /// Publish a guest port on the host: host listens on `listen`, connects to
    /// `connect` in the guest. Addresses are `tcp:IP:PORT`.
    pub fn host(listen: impl Into<String>, connect: impl Into<String>) -> PortSpec {
        PortSpec {
            bind: PortBind::Host,
            listen: listen.into(),
            connect: connect.into(),
            ..Default::default()
        }
    }

    /// Reach a host service from the guest: guest listens on `listen`, host connects to `connect`.
    pub fn guest(listen: impl Into<String>, connect: impl Into<String>) -> PortSpec {
        PortSpec {
            bind: PortBind::Guest,
            listen: listen.into(),
            connect: connect.into(),
            ..Default::default()
        }
    }
}

impl PortSpec {
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }
    pub fn search(mut self, n: u16) -> Self {
        self.search = Some(n);
        self
    }
}

impl SandboxSpec {
    pub fn new(name: impl Into<String>, image: impl Into<String>) -> Self {
        SandboxSpec {
            name: Some(name.into()),
            image: image.into(),
            ..Default::default()
        }
    }
    pub fn cpus(mut self, cpus: impl ToString) -> Self {
        self.cpus = Some(cpus.to_string());
        self
    }
    pub fn cpuset(mut self, set: impl Into<String>) -> Self {
        self.cpuset = Some(set.into());
        self
    }
    pub fn memory(mut self, m: impl Into<String>) -> Self {
        self.memory = Some(m.into());
        self
    }
    pub fn storage(mut self, pool: impl Into<String>) -> Self {
        self.storage = Some(pool.into());
        self
    }
    pub fn idmap(mut self, idmap: IdmapSpec) -> Self {
        self.idmap = Some(idmap);
        self
    }
    pub fn privileged(mut self, p: bool) -> Self {
        self.privileged = Some(p);
        self
    }
    pub fn label(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.labels.insert(k.into(), v.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }
    /// Restrict the sandbox's network: see [`crate::egress::EgressSpec`].
    pub fn egress(mut self, e: crate::egress::EgressSpec) -> Self {
        self.egress = Some(e);
        self
    }
    /// Mount `vol` at `guest_path`.
    pub fn volume(mut self, guest_path: impl Into<String>, mut vol: VolumeSpec) -> Self {
        vol.target = guest_path.into();
        self.volumes.push(vol);
        self
    }
    pub fn port(mut self, p: PortSpec) -> Self {
        self.ports.push(p);
        self
    }
    pub fn ready(mut self, checks: Vec<ReadyCheck>) -> Self {
        self.ready = Some(checks);
        self
    }
    pub fn ready_timeout(mut self, t: impl Into<String>) -> Self {
        self.ready_timeout = Some(t.into());
        self
    }
    pub fn user(mut self, u: impl Into<String>) -> Self {
        self.user = Some(u.into());
        self
    }
    pub fn working_dir(mut self, c: impl Into<String>) -> Self {
        self.working_dir = Some(c.into());
        self
    }
    pub fn raw_config(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.raw_config.insert(k.into(), v.into());
        self
    }
    pub fn raw_device(mut self, name: impl Into<String>, props: BTreeMap<String, String>) -> Self {
        self.raw_devices.insert(name.into(), props);
        self
    }
}
