# Apps, stacks, builds, templates

**Apps** (Dokploy-style). A project has environments; each environment runs
as one stack, `<project>-<env>`.

- `project_create`, `environment_create`, then `app_create`: an image, or a
  git repository and a builder.
- `app_env_set`: `.env` text; `KEY=${{secret.NAME}}` for secrets.
- `app_deploy` (`wait: true` to block), `app_deployments`,
  `app_deployment_log`, `app_rollback`, `app_get`, `app_list`, `app_update`.
- `app_apply`: a YAML definition that creates or updates; `dry_run: true`
  first shows the diff. `app_export` gives an app's YAML back.
- A redeploy always replaces the instances, even with unchanged settings.

**Stacks**: long-running services from a compose file, with
`deploy.replicas`, health-checked replicas, a load balancer on published
ports and rolling updates (`deploy.update_config.order: start-first` for no
downtime).

- `stack_deploy` (compose YAML; `dry_run: true` first, `wait: true` to
  block), `stack_validate`, `stack_status`, `stack_logs`, `stack_scale`,
  `stack_redeploy`, `stack_rollback` (`to`: a kept deployment),
  `stack_deployments`, `stack_export`, `stack_remove`.
- `stack_env_set`: `.env` text the file's `${VAR}` resolves against at deploy.
- `stack_domains_set`: route a domain to a service. `ingress_status` shows
  certificates and conflicts.

Deploys return immediately unless `wait: true`; poll `stack_status`.

**Builds**: `build_run` builds a repository into `registry:APP:TAG` in a fresh
sandbox; `build_list`, `build_logs`, `registry_list`, `registry_gc`. A stack
or app image the registry does not have is refused.

**Templates** (one-click apps): `template_list`, `template_get`, then
`template_deploy` (`dry_run: true` first). `template_instance_list`,
`template_instance_delete`.

**Previews**: `preview_list`, `preview_get`, `preview_log`,
`preview_redeploy`, `preview_delete`.
