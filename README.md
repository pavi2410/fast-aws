# fast-aws

A fast, keyboard-driven terminal UI for AWS: browse resources, follow links
between them, and read logs without the console or remembering CLI commands.

> ⚠ **Some views make AWS calls that cost money or read data.** They are all
> opt-in or on a key press, but read [Costs and data access](#costs-and-data-access)
> before using them on a busy account.

## Running

```sh
cargo run --release --              # starts on a list of every resource type
cargo run --release -- ec2          # or straight into one
cargo run --release -- lambda --profile my-sso-profile --region eu-west-1
```

| Flag | Meaning |
|---|---|
| `-p, --profile NAME` | AWS profile (default: `AWS_PROFILE`, else `default`, else the first profile) |
| `-r, --region REGION` | Region (default: `AWS_REGION`, then the profile's region) |
| `--read-only` | Never offer actions that change anything |
| `--metrics` | Show sparklines (**billed**, see below) |
| `--no-metrics` | No sparklines, even if the config turns them on |

Credentials come from the usual AWS config: SSO (`[sso-session]` profiles; `L`
runs `aws sso login --sso-session …` when the session expires), `aws login`
console sessions, assume-role, `credential_process` and static keys. Profiles that
need an MFA code aren't supported yet.

## Keys

| Key | Does |
|---|---|
| `:` | Jump to a resource type (`:ec2`, `:s3`, `:fn`, …); `:q` quits |
| `/` | Filter the list |
| `Enter` | Open: drill in (bucket → objects, log group → streams, role → permissions) or describe |
| `d` | Describe the selected item (`Tab` switches fields / raw) |
| `g` | Go to a related resource (a function's role or logs, an instance's security groups, …) |
| `r` | Refresh · `n` load more (lists pause after 5 pages) |
| `P` / `R` | Switch profile / region |
| `Z` | Times in local time / UTC |
| `Esc`, `q` | Back; `q` quits from the top · `Ctrl-C` quits |
| `?` | Help |

Some rows have keys of their own, shown at the left of the footer:

| Key | On | Does |
|---|---|---|
| `S` | IAM role or user | Simulate "can it do X?" |
| `I` | Log group | Logs Insights query (**billed**) |
| `Q` | DynamoDB table | Look up items by partition key |
| `v` | Secret | Reveal its value (you type `reveal` to confirm) |

## Supported resources

All read-only (list and describe). Nothing in fast-aws changes AWS resources
today.

| Service | What you can see |
|---|---|
| EC2 | Instances (with optional CPU sparkline), security groups, VPCs → subnets, subnets, EBS volumes, owned AMIs, key pairs, Elastic IPs |
| S3 | Buckets (each in its own region) → objects by folder |
| Lambda | Functions (with optional errors sparkline), event source mappings; follow a function's logs live |
| CloudWatch Logs | Log groups → streams → events; follow a group live; Logs Insights |
| CloudWatch | Metric and composite alarms |
| IAM | Roles, users (both → effective permissions, and the simulator), groups, customer-managed policies |
| API Gateway | REST, HTTP and WebSocket APIs → routes with their integrations |
| CloudFront | Distributions → invalidations |
| DynamoDB | Tables; items by partition key |
| RDS | Aurora clusters and DB instances (optional CPU sparkline), snapshots |
| SSM Parameter Store | Parameters (SecureString values are never decrypted or shown) |
| Secrets Manager | Secrets (values only after `v` + `reveal`) |
| EventBridge | Buses → rules, Scheduler schedules |
| Cognito | User pools → users, identity pools |
| SES | Account status (sandbox, quota), identities |
| Cost Explorer | Month-to-date cost by service (**billed**) |

## Costs and data access

Most of fast-aws uses free AWS List/Describe/Get calls. These don't, or read
data you may not want on screen. Check the current AWS prices for your region.

| Feature | What it calls | Cost / impact |
|---|---|---|
| **Sparklines** (`--metrics` or `metrics = true`) | CloudWatch `GetMetricData` for up to 60 rows around the cursor, reused for 10 minutes | **Billed per metric requested.** Off by default. |
| **Cost Explorer** (`:cost`) | `GetCostAndUsage`, once per open, never refreshed on its own (`r` re-runs it) | **Billed per request** (about $0.01 each). |
| **Logs Insights** (`I` on a log group) | `StartQuery` / `GetQueryResults`; the query is stopped if you leave the view | **Billed per GB of logs scanned.** Keep the time range short (`-15m …`). |
| **Follow live** (`g` → follow live) | `FilterLogEvents` every 2 s, at most 2 calls a poll | Uses a shared per-account rate limit (5–25 calls/s depending on region), and puts log contents on screen. Not the billed Live Tail API. |
| **DynamoDB items** (`Q`) | `Query`, 50 items a call, 5 calls before pausing | Consumes the table's read capacity; shows item data. No Scan. |
| **Secret values** (`v`, then `reveal`) | `GetSecretValue` | **Billed per call**, recorded in CloudTrail, and shows the secret on screen. Never cached. |
| **IAM permissions and simulator** | IAM `Get*`/`List*`, `SimulatePrincipalPolicy` | Free, but IAM's rate limits are low; at most 4 calls at a time. |

## Config file

`~/.config/fast-aws/config.toml` (or under `$XDG_CONFIG_HOME`). Unknown keys stop
startup, so typos don't go unnoticed.

```toml
read_only = true          # every profile (or --read-only)
metrics = true            # sparklines; billed (or --metrics)
sparkline = "bars"        # or "dots" (the default)
borders = true            # frame lists and details
timezone = "utc"          # or "local" (the default); Z switches in the app

[profiles.my-readonly-profile]
read_only = true

[profiles.prod-admin]
prod = true               # red PROD badge
```

## Building

Rust 1.98 (pinned in `rust-toolchain.toml`). The AWS SDK crates are large: a
cold build takes a few minutes and up to about 9 GiB of memory with 6 parallel
jobs; use `cargo build -j 6` (or fewer) if a build is killed. Day-to-day
rebuilds take a few seconds.

The design and plans are in [`docs/plan/`](docs/plan/README.md).
