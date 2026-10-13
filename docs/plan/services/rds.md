# RDS (+ Aurora)

## Crates & scope

- Crate: `aws-sdk-rds` (it includes an `auth_token` module for IAM database auth tokens)
- Scope: regional
- **Instances and Aurora clusters are separate API calls.** The main list merges
  them into one tree:

```
 NAME                     ENGINE            CLASS          STATUS      ROLE     AZ
▾orders-aurora            aurora-postgres   —              available   cluster  —
   orders-aurora-1        aurora-postgres   db.r6g.large   available   writer   eu-west-1a
   orders-aurora-2        aurora-postgres   db.r6g.large   available   reader   eu-west-1b
 legacy-mysql             mysql 8.0         db.t3.medium   stopped     —        eu-west-1a
```

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `rds:cluster` | `rds`, `db`, `aurora` | `DescribeDBClusters` | same, filtered |
| `rds:instance` | (merged in `rds`) | `DescribeDBInstances` | same, filtered |
| `rds:snapshot` | `snap` | `DescribeDBSnapshots` + `DescribeDBClusterSnapshots` (merged) | |
| `rds:event` | (drill-in) | `DescribeEvents` (last 24 h, for the selected source) | |
| `rds:logfile` | (drill-in) | `DescribeDBLogFiles` | `DownloadDBLogFilePortion` |
| `rds:paramgroup` | `pg` | `DescribeDBParameterGroups` / `DescribeDBClusterParameterGroups` | `DescribeDBParameters` (default: `Source=user`, i.e. changed values only) |
| `rds:proxy` | `proxy` | `DescribeDBProxies` | `DescribeDBProxyTargets` |

## Columns

`NAME` · `ENGINE` (+ version) · `CLASS` · `STATUS` · `ROLE` · `AZ` / `MULTI-AZ` · `ENDPOINT` · `CPU` (sparkline) · `CONNS` (sparkline) · `STORAGE` (free / allocated)

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `C` | Connect: run `psql` / `mysql` with the endpoint, port and user pre-filled (password from the managed secret, or an IAM auth token) | shell out; `auth_token` / `secretsmanager:GetSecretValue` | safe | `psql "host={ep} port={port} user={u} dbname={db} sslmode=require"` |
| `u` | Copy endpoint / connection string | — | safe | |
| `l` | Log files → log pane (`DownloadDBLogFilePortion`, follow mode polls) | `DescribeDBLogFiles`, `DownloadDBLogFilePortion` | safe | |
| `E` | Recent events | `DescribeEvents` | safe | |
| `x→start/stop` | Start / stop instance or cluster | `Start/StopDBInstance`, `Start/StopDBCluster` | mutating | `aws rds stop-db-instance --db-instance-identifier {id}` |
| `x→reboot` | Reboot (with failover option for Multi-AZ) | `RebootDBInstance` | mutating | |
| `x→failover` | Fail over an Aurora cluster | `FailoverDBCluster` | mutating | |
| `x→snapshot` | Create a manual snapshot (name prompt) | `CreateDBSnapshot` / `CreateDBClusterSnapshot` | mutating | |
| `x→delete-snap` | Delete a manual snapshot | `DeleteDBSnapshot` / `DeleteDBClusterSnapshot` | destructive | |

Deleting or modifying instances (`ModifyDBInstance`, `DeleteDBInstance`) is **out of
scope** for v1. These changes are high-risk and belong in IaC.

## Connect (`C`) details

- Pick the client binary by engine family (`psql` for Postgres, `mysql`/`mariadb`
  for MySQL/MariaDB). If it isn't on `PATH`, say so.
- Choose credentials in this order:
  1. managed master secret (`MasterUserSecret` → Secrets Manager)
  2. IAM auth token (if `IAMDatabaseAuthenticationEnabled`)
  3. password prompt
- Pass the password through the environment (`PGPASSWORD` / `MYSQL_PWD`) of the
  child process only, never on the command line.
- Private endpoints: if the DB isn't reachable, offer an SSM port-forward through a
  chosen EC2 instance
  (`AWS-StartPortForwardingSessionToRemoteHost`, needs `session-manager-plugin`).
  This is a later phase.
- Uses the TUI suspend/resume helper from [EC2](ec2.md#gotchas).

## Relations

- cluster ↔ member instances
- instance → VPC, subnet group, security groups
- instance/cluster → parameter group, option group
- instance/cluster → KMS key
- instance/cluster → managed master secret (Secrets Manager)
- instance/cluster → RDS proxy → targets
- instance/cluster → snapshots
- instance → log exports in CloudWatch Logs (`/aws/rds/instance/{id}/{log}`)
- instance → CloudWatch metrics (`CPUUtilization`, `DatabaseConnections`,
  `FreeStorageSpace`, `ReadLatency`/`WriteLatency`)

## IAM permissions

- **Read:** `rds:Describe*`, `rds:DownloadDBLogFilePortion`, `cloudwatch:GetMetricData`; for connect: `rds-db:connect` (IAM auth) or `secretsmanager:GetSecretValue`
- **Write:** `rds:Start*`, `rds:Stop*`, `rds:RebootDBInstance`, `rds:FailoverDBCluster`, `rds:Create*Snapshot`, `rds:Delete*Snapshot`

## Gotchas

- `DescribeDBInstances` also returns Aurora instances, and `DescribeDBClusters` also
  returns Multi-AZ DB clusters (non-Aurora). Group them using `DBClusterIdentifier`,
  and don't show an instance twice.
- `DescribeDBInstances` also returns DocumentDB and Neptune instances. Show them,
  marked by engine, but no connect action for them in v1.
- A stopped instance restarts automatically after 7 days. Show a "restarts in Nd"
  hint on stopped rows.
- Log file downloads are paginated by `Marker` and limited in size per call. Follow
  mode keeps the last marker and polls.
- Performance Insights (`aws-sdk-pi`) is out of scope for v1; the sparklines cover
  basic health.
