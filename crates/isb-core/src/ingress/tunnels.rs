//! The Cloudflare Tunnel provider's side of the applier: each tunnel org's
//! cloudflared stack, and its tunnel synced through the API ([`cloudflare`]).

use super::*;

/// Who deploys the tunnel stacks.
const TUNNEL_DEPLOYER: &str = "isb ingress";

impl Manager {
    /// Run each tunnel org's cloudflared stack, and sync its tunnel through
    /// the API when the org gave a token for it.
    #[expect(
        clippy::too_many_lines,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    pub(super) fn tunnels(
        &self,
        orgs: &BTreeMap<OrgId, OrgIngress>,
        served: &[Served],
        listeners: &[TunnelListener],
    ) {
        let Some(ctl) = self.ctl.get() else { return };
        let mut status: BTreeMap<OrgId, TunnelStatus> = self.state.lock().unwrap().tunnels.clone();
        for (org, oi) in orgs {
            if !oi.tunnel {
                continue;
            }
            let ts = status.entry(org.clone()).or_insert_with(|| TunnelStatus {
                org: org.to_string(),
                ..Default::default()
            });
            ts.error = None;
            ts.origin = listeners
                .iter()
                .find(|l| l.org == *org)
                .map(|l| format!("http://{}", l.listen));
            if ts.origin.is_none() {
                ts.error = Some("the org's bridge has no IPv4 address".into());
                continue;
            }
            if let Err(e) = self.ensure_tunnel_stack(ctl, org) {
                ts.error = Some(e.to_string());
                ts.stack = false;
                continue;
            }
            ts.stack = true;
            let api_token = self
                .secrets
                .get(org, cloudflare::API_TOKEN_SECRET)
                .ok()
                .map(|(v, _)| String::from_utf8_lossy(&v).trim().to_string());
            ts.api_managed = api_token.is_some();
            let Some(api_token) = api_token else { continue };
            let mut hosts: Vec<String> = served
                .iter()
                .filter(|s| s.via == Via::Tunnel(org.clone()))
                .map(|s| s.route.host.clone())
                .collect();
            hosts.sort();
            hosts.dedup();
            let due = ts.synced_hosts != hosts
                || crate::stack::now_secs().saturating_sub(ts.last_sync_at) > RESYNC;
            if !due {
                continue;
            }
            let r = (|| -> Result<cloudflare::SyncReport> {
                let (tok, _) = self.secrets.get(org, cloudflare::TOKEN_SECRET)?;
                let t = cloudflare::parse_token(&String::from_utf8_lossy(&tok))?;
                let api = cloudflare::Api::new(&self.cfg.cloudflare_api, &api_token);
                cloudflare::sync(
                    &api,
                    &cloudflare::SyncPlan {
                        account: oi.account.clone().unwrap_or(t.account),
                        tunnel: t.tunnel,
                        zone: oi.zone.clone(),
                        hosts: hosts.clone(),
                        origin: ts.origin.clone().unwrap_or_default(),
                    },
                )
            })();
            ts.last_sync_at = crate::stack::now_secs();
            match r {
                Ok(rep) => {
                    if !(rep.created.is_empty() && rep.updated.is_empty() && rep.deleted.is_empty())
                    {
                        ctl.note(
                            "info",
                            &crate::stack::qualified(org, cloudflare::TUNNEL_STACK),
                            format!(
                                "cloudflare tunnel synced: {} rules; DNS created {:?}, updated {:?}, deleted {:?}",
                                rep.ingress_rules, rep.created, rep.updated, rep.deleted
                            ),
                        );
                    }
                    ts.synced_hosts = hosts;
                    ts.last_sync = Some(rep);
                }
                Err(e) => {
                    ts.error = Some(e.to_string());
                    ctl.note(
                        "warn",
                        &crate::stack::qualified(org, cloudflare::TUNNEL_STACK),
                        format!("cloudflare tunnel sync failed: {e}"),
                    );
                }
            }
        }
        // Orgs that left the tunnel provider: their cloudflared goes.
        let gone: Vec<OrgId> = status
            .keys()
            .filter(|o| !orgs.get(*o).is_some_and(|oi| oi.tunnel))
            .cloned()
            .collect();
        for o in gone {
            let q = crate::stack::qualified(&o, cloudflare::TUNNEL_STACK);
            if ctl
                .definition(&q)
                .is_ok_and(|d| d.deployed_by == TUNNEL_DEPLOYER)
            {
                let _ = ctl.remove(&q, false, Duration::from_secs(1));
                ctl.note(
                    "info",
                    &q,
                    "org left the cloudflare-tunnel provider: removed".into(),
                );
            }
            status.remove(&o);
        }
        self.state.lock().unwrap().tunnels = status;
    }

    /// Deploy (or keep) the org's cloudflared stack.
    fn ensure_tunnel_stack(&self, ctl: &Controller, org: &OrgId) -> Result<()> {
        self.secrets
            .inspect(org, cloudflare::TOKEN_SECRET)
            .map_err(|_| {
                Error::invalid(format!(
                    "no secret {} in org {org}: isb secret create {} --org {org}",
                    cloudflare::TOKEN_SECRET,
                    cloudflare::TOKEN_SECRET
                ))
            })?;
        let file = cloudflare::tunnel_stack();
        let q = crate::stack::qualified(org, cloudflare::TUNNEL_STACK);
        if let Ok(d) = ctl.definition(&q) {
            if d.file == file {
                return Ok(());
            }
            if d.deployed_by != TUNNEL_DEPLOYER {
                return Err(Error::invalid(format!(
                    "stack {} exists and is not isb's; remove it",
                    cloudflare::TUNNEL_STACK
                )));
            }
        }
        let mut def = StackDef {
            source: None,
            domains: Default::default(),
            name: cloudflare::TUNNEL_STACK.into(),
            org: org.clone(),
            file,
            base_dir: self.dir.clone(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: crate::stack::now_secs(),
            deployed_by: TUNNEL_DEPLOYER.into(),
            previous: None,
        };
        ctl.validate(&def)?;
        def.secrets = crate::stack::secrets::bind(
            &self.secrets,
            org,
            &def.name,
            &def.file,
            &BTreeMap::new(),
            false,
        )?;
        ctl.deploy(def)?;
        ctl.note(
            "info",
            &q,
            "cloudflared deployed for the org's tunnel".into(),
        );
        Ok(())
    }
}
