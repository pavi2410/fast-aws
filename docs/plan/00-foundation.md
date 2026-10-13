# Foundation

Everything here is shared by every service. A service integration should mostly be
*data*: columns, API calls, actions and relations plugged into these pieces.

## Stack

| Concern | Choice |
|---|---|
| UI | `ratatui` 0.30 + `crossterm` |
| Async runtime | `tokio` |
| AWS | `aws-config` 1.x + one `aws-sdk-*` crate per service (see [crates](#crates-checked-on-cratesio)) |
| Config | TOML at `~/.config/fast-aws/config.toml` |
| Clipboard | `arboard` (with an OSC 52 fallback for SSH sessions) |

## Crates (checked on crates.io)

Checked with `cargo search` on 2026-10-13. These are the crate names; check the
versions again when they're added.

| Purpose | Crates |
|---|---|
| Core | `aws-config` 1.12, `aws-sdk-sts`, `aws-sdk-sso`, `aws-sdk-ssooidc` |
| In-scope services | `aws-sdk-ec2`, `aws-sdk-s3`, `aws-sdk-lambda`, `aws-sdk-apigateway`, `aws-sdk-apigatewayv2`, `aws-sdk-cloudfront`, `aws-sdk-cloudwatchlogs`, `aws-sdk-cloudwatch`, `aws-sdk-costexplorer`, `aws-sdk-iam`, `aws-sdk-cognitoidentityprovider`, `aws-sdk-cognitoidentity`, `aws-sdk-eventbridge`, `aws-sdk-scheduler`, `aws-sdk-sesv2`, `aws-sdk-secretsmanager`, `aws-sdk-ssm`, `aws-sdk-dynamodb`, `aws-sdk-rds` |
| Supporting | `aws-sdk-acm` (CloudFront certs), `aws-sdk-pricing` (cost hints) |
| Later / optional | `aws-sdk-pipes`, `aws-sdk-pi`, `aws-sdk-resourcegroupstagging`, `aws-sdk-s3-transfer-manager` (0.x) |
| UI and testing | `ratatui` 0.30, `crossterm`, `arboard` 3.x, `aws-smithy-mocks` 0.3, `insta` 1.x |

## Workspace layout (to keep build times manageable)

Pulling in 20+ `aws-sdk-*` crates is slow to compile. Split them so that editing one
service rebuilds only that crate, and so services can be switched off.

```
crates/
  core/          Resource trait, client factory, cache, task runner, errors, config
  ui/            app loop, shared widgets (table, browser, viewer, log pane, dialogs)
  svc-ec2/       one crate per service, depending on core + its aws-sdk crate(s)
  svc-s3/
  ...
  fast-aws/      the binary; enables services through Cargo features (all on by default)
```

- Each `svc-*` crate registers its resource kinds with a registry in `core`.
- Turn on the SDK's `rt-tokio` + `behavior-version-latest` features and nothing
  more than needed.
- Use a dev profile with `debug = "line-tables-only"`; consider `mold`/`lld` as the linker.

## The `Resource` abstraction

A *resource kind* is one list-able thing, such as `ec2:instance`, `s3:object` or
`lambda:function`. Sketch (object-safe, so the registry can hold `Box<dyn ResourceKind>`):

```rust
#[async_trait]
pub trait ResourceKind: Send + Sync {
    fn id(&self) -> &'static str;                // "ec2:instance"
    fn aliases(&self) -> &'static [&'static str]; // ["ec2", "i", "instances"]
    fn scope(&self) -> Scope;                     // Regional | Global | Pinned("us-east-1")
    fn columns(&self) -> &[Column];
    fn parent(&self) -> Option<&'static str>;     // e.g. s3:object -> s3:bucket

    async fn list(&self, ctx: &Ctx, q: &ListQuery) -> Result<Page<Row>>;
    async fn describe(&self, ctx: &Ctx, id: &ResourceId) -> Result<Detail>;

    fn actions(&self) -> &[ActionSpec];
    fn relations(&self, row: &Row) -> Vec<RelationRef>;
}

pub struct ActionSpec {
    pub key: char,
    pub label: &'static str,
    pub safety: Safety,               // Safe | SideEffect | Mutating | Destructive
    pub cli_template: &'static str,   // "aws ec2 stop-instances --instance-ids {id}"
    pub confirm: Confirm,             // None | YesNo | TypeName
}
```

- `ListQuery` carries the parent (bucket, log group, table…), a filter, and a
  continuation token.
- `Row` is a small view model (an ID plus cell values), not the SDK type. The full
  SDK object is fetched by `describe`.
- `cli_template` renders the equivalent `aws` command for every action. This is the
  "never memorise the CLI again" feature, and it costs one string per action.

### Gotcha: SDK types don't implement `Serialize`

`aws-sdk-*` output types don't derive serde by default. For the describe view:

1. **Spike first:** check whether the unstable `serde-serialize` feature
   (`--cfg aws_sdk_unstable`) is still available and usable.
2. **Fallback:** hand-write `Detail` mappings for the important fields, and show the
   rest by pretty-printing the `Debug` output.

## Client factory and credentials

- Clients are cached by `(profile, region, service)`. They are cheap to clone and
  built lazily the first time they are needed.
- `aws-config` already handles env vars, shared config/credentials files, SSO
  (token cache from `aws sso login`), `assume_role`/`source_profile`,
  `credential_process`, and IMDS.
- When the app starts, and when you switch profile, call `sts:GetCallerIdentity`.
  It shows the account ID and ARN in the header and proves the credentials work.
- **SSO expiry:** detect expired-token errors and show an inline banner saying
  *"SSO session expired: press `L` to run `aws sso login --profile X`"*.
  Later, this login could run inside the TUI using the `ssooidc` device flow.
- **MFA (`mfa_serial`):** check whether `aws-config` can prompt for a code. If not,
  give the user the profile's MFA prompt in a dialog using a custom credentials provider.

### Scope rules

| Scope | Services | Behaviour |
|---|---|---|
| Regional | most services | use the current region |
| Global | IAM, CloudFront | the region switcher is greyed out in these views |
| Pinned | Cost Explorer (`us-east-1`), CloudFront ACM certificates (`us-east-1`) | always use the fixed region |
| Per-item | S3 buckets | each bucket's region comes from `ListBuckets` (`BucketRegion`), so object calls use a client for that region |

### Multi-region and multi-profile

- `R` → "all regions" runs the list call against every enabled region concurrently
  (bounded) and adds a `REGION` column.
- Get the region list from `ec2:DescribeRegions`, cached for a day.

## Async task model

```
UI thread (ratatui draw + input) ──Cmd──▶ dispatcher ──spawn──▶ tokio tasks (SDK calls)
        ▲                                                         │
        └──────────────────── Event (data / error / progress) ◀───┘
```

- The UI thread never awaits AWS calls.
- Each view owns a `TaskGroup`. Leaving the view aborts its tasks, so a list you have
  scrolled away from stops paginating.
- **Pagination:** stream pages into the table as they arrive and show a
  `loading… 1,200 so far` counter. Filtering works on rows already loaded, and a
  server-side filter is used when the API has one.
- **Throttling:** use `RetryConfig::adaptive()` (verified to exist in
  `aws-smithy-types`), plus a per-service concurrency semaphore (start at 4–8).
  This matters for the SSM, Cognito and IAM APIs, which have low rate limits.

## Cache (stale-while-revalidate)

- Key: `(account, region, kind, parent, query)`.
- Show cached rows instantly (marked *stale* in the header), refresh in the
  background, then swap in the result while keeping the cursor on the same ID.
- In memory by default. Disk-backed for expensive or slow data: Cost Explorer,
  the region list, and the EC2 instance-type catalogue.
- Default TTLs are set per kind. Auto-refresh is on for cheap lists (for example
  every 10 s while visible), **off** for anything billed per request
  (Cost Explorer, CloudWatch `GetMetricData`, Logs Insights).

## Errors

Each view is in one of these states: `Loading`, `Ready`, `Stale`, or `Error`. Errors
are shown *inside the pane*, never as a crash or an empty table.

- **AccessDenied:** show the missing action when the error names it, e.g.
  *"missing `ec2:DescribeInstances` on `arn:aws:iam::…:role/dev`"*.
- **Throttled:** show it and keep retrying with backoff.
- **Region not enabled / endpoint missing:** say so, and suggest switching region.
- An error in one pane (for example a metrics sparkline) never blocks the main list.

## Safety

- **Read-only mode** comes from `--read-only` or from per-profile config. Mutating and
  destructive actions are hidden, not just disabled. `SideEffect` actions stay
  available behind a confirmation (see the safety classes in the [README](README.md#service-doc-template)).
- **Prod marking:** a per-profile `color = "red"` and `prod = true` tint the header.
  Prod profiles default to read-only and can be unlocked for the session with `:unlock`. It's a typed command,
  not a single key, so it can't be pressed by accident.
- `destructive` actions ask you to type the resource name. Bulk actions show the full
  list plus the CLI equivalent before running.
- EC2 and a few other APIs support `DryRun`. Use it to check permissions before
  showing an action as available.

## Shared widgets (`crates/ui`)

Build each of these once and reuse it across services:

| Widget | Used by |
|---|---|
| **Table** (sort, filter, column picker, multi-select) | everything |
| **Miller-column browser** | S3 prefixes, SSM parameter paths, (later) Secrets name paths |
| **Detail viewer** (YAML/JSON, folding, search, copy a path) | every `d` |
| **Log pane** (follow, filter, highlight, wrap toggle, jump to time) | CloudWatch Logs, Lambda, API GW, RDS logs, EC2 console output |
| **Sparkline / chart** | EC2, Lambda, RDS, DynamoDB, CloudWatch, Cost Explorer |
| **Editor round trip** (`$EDITOR` → validate → diff → apply) | Lambda env, SSM params, secrets, IAM policies, DynamoDB items |
| **Secret field** (masked, reveal on key, clipboard auto-clear) | Secrets Manager, SSM SecureString, Cognito client secret, RDS master secret |
| **Confirm dialog** (y/n or type-to-confirm, shows the CLI command) | all actions |

## Config file sketch

```toml
[ui]
refresh_secs = 10
clipboard_clear_secs = 30

[profiles.prod-admin]
prod = true
color = "red"
read_only = true

[[bookmarks]]
name = "staging web"
target = ":ec2 /env=staging role=web"
profile = "staging"
region = "eu-west-1"
```

## Spikes to run before building on assumptions

This is the single list of open technical questions. [Phasing](phasing.md) Phase 0
works through it, and service docs link here instead of keeping their own lists.

| # | Question | Raised in | Fallback if it fails |
|---|---|---|---|
| 1 | Is the unstable `serde-serialize` feature usable for SDK types? | [above](#gotcha-sdk-types-dont-implement-serialize) | hand-written `Detail` mappings + `Debug` |
| 2 | How does the `start_live_tail` event receiver behave in a tokio task? (The client method exists.) | [Logs](services/cloudwatch-logs.md) | poll `FilterLogEvents` |
| 3 | Can `aws-config` prompt for MFA (`mfa_serial`)? | [above](#client-factory-and-credentials) | custom credentials provider with a TUI prompt |
| 4 | Does the `aws-sdk-rds` `auth_token` module (it exists) generate usable IAM DB tokens? | [RDS](services/rds.md) | managed secret or password prompt only |
| 5 | Can the TUI suspend and resume cleanly around a child process (`$EDITOR`, `ssh`, `session-manager-plugin`, `psql`)? | [EC2](services/ec2.md#gotchas) | — (needed; must work) |
| 6 | Is `aws-sdk-s3-transfer-manager` (0.x) ready for multipart upload/download? | [S3](services/s3.md#gotchas) | small hand-written multipart helper |
| 7 | With `PutParameter` `Overwrite=true`, does leaving out `KeyId` re-encrypt a SecureString with `aws/ssm`? | [SSM](services/ssm-parameter-store.md#gotchas) | always send `KeyId` (planned anyway) |
| 8 | Cold build time of the full workspace, and of a single-service rebuild | [above](#workspace-layout-to-keep-build-times-manageable) | fewer default features, faster linker |
