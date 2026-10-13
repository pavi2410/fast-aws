# Foundation

Everything here is shared by every service. A service integration should mostly be
*data*: columns, API calls, actions and relations plugged into these pieces.

## Stack

| Concern | Choice |
|---|---|
| UI | `ratatui` 0.30 + `crossterm` |
| Async runtime | `tokio` |
| AWS | `aws-config` 1.x + one `aws-sdk-*` crate per service (crate names verified on crates.io, Oct 2026) |
| Config | TOML at `~/.config/fast-aws/config.toml` |
| Clipboard | `arboard` (with an OSC 52 fallback for SSH sessions) |

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
    pub safety: Safety,               // Safe | Mutating | Destructive
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
  destructive actions are hidden, not just disabled.
- **Prod marking:** a per-profile `color = "red"` and `prod = true` tint the header.
  Prod profiles default to read-only and can be unlocked for the session with `U`.
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

1. Serde for SDK types (see above).
2. `cloudwatchlogs::start_live_tail` event-stream handling. The client method exists;
   confirm how its event receiver behaves in a tokio task.
3. MFA prompting in `aws-config`.
4. The `aws-sdk-rds` `auth_token` module (it exists) for IAM DB auth tokens.
5. Cold build time of the full workspace, and of a single-service rebuild.
