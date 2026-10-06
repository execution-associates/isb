# Data: databases, backups, volumes, jobs, secrets

**Databases** are apps: `database_create` (Postgres, MySQL, MariaDB, MongoDB,
Redis, with generated credentials), `database_get` (connection details),
`database_list`.

**Backups** go to S3-compatible destinations on a cron schedule:
`backup_destination_create`, `backup_destination_test`, `backup_create`,
`backup_run`, `backup_runs`, `backup_run_log`, `backup_list`,
`backup_restore`.

**Volumes**: `volume_create`, `volume_list`, `volume_get`, `volume_delete`
(refused while attached). Snapshots: `volume_snapshot_create`,
`volume_snapshot_list`, `volume_snapshot_schedule`, `volume_snapshot_runs`;
`volume_restore` stages a restore, `volume_restore_list`,
`volume_restore_discard`.

**Jobs** run a command against an app on a cron schedule: `job_create`,
`job_run`, `job_runs`, `job_run_log`, `job_update`, `job_delete`.

**Secrets**: `secret_create`, `secret_set` (values are base64; a new version
rolls the stacks using it), `secret_list` (no values), `secret_inspect`,
`secret_get` (the value: members and up), `secret_delete`. Reference them
from apps as `${{secret.NAME}}`, from compose as `{secret: NAME}`.
