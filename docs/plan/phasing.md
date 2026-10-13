# Phasing

The order is: foundation, then **read-only** browsing (most value first), then
actions, then power features. Each phase ends with something usable every day.
Mutating actions arrive only after the safety system (read-only mode, prod marking,
confirmations) has been used for a while.

## Phase 0: Spikes (time-boxed, about 2 days)

These resolve the open questions in [foundation § spikes](00-foundation.md#spikes-to-run-before-building-on-assumptions):

- [ ] Serde for SDK types (unstable `serde-serialize`), or decide on hand-written `Detail` mappings
- [ ] `StartLiveTail` event stream inside tokio, plus the `FilterLogEvents` polling fallback
- [ ] MFA prompt support in `aws-config`
- [ ] Cold and incremental build times with the workspace layout
- [ ] Suspend/resume of the TUI around a child process (`$EDITOR`, `ssh`, `session-manager-plugin`)

**Exit:** a short decision note added to `00-foundation.md` for each spike.

## Phase 1: Walking skeleton

- Workspace layout: `core`, `ui`, `svc-ec2`, `fast-aws`
- Client factory, `GetCallerIdentity` header, profile switcher (`P`) and region switcher (`R`)
- Task model: background list, pagination streaming, cancellation when a view is left
- Table widget with filter and sort; detail viewer; error-in-pane states
- Command palette (`:`)
- **EC2 instances**, read-only: list + describe

**Exit:** `fast-aws` opens to EC2 instances for the default profile in under 1 s when
cached. Switching profile/region works. AccessDenied shows inside the pane.

## Phase 2: Read-only core (the daily drivers)

These four account for most console visits, and Lambda depends on Logs.

- EC2 (complete): SGs, VPCs, subnets, volumes, AMIs, sparkline widget
- CloudWatch Logs: groups, streams, **log pane** (live + range), Insights (read)
- CloudWatch metrics: sparkline/chart widget, alarms list
- Lambda: list with error sparklines, config panel, `l` logs
- S3: **Miller-column browser**, preview, download, presigned URL (all safe)
- Cache (stale-while-revalidate), with disk cache for the region list
- `g` relations for kinds F and N among these services
- `c` copies the CLI command

**Exit:**
- Relation chain 4 (alarm → resource → metrics + logs) from
  [relations](relations.md#most-valuable-chains-used-as-acceptance-tests-in-phasing)
  works.
- The Lambda → log pane (filtered to `ERROR`) part of chain 1 works. The API GW part
  waits for Phase 3.
- A week of daily use without opening the console for these services.

## Phase 3: Read-only breadth

In rough order of value:

1. IAM (effective permissions view, trust policy, simulator)
2. API Gateway (unified v1/v2, route tree)
3. SSM Parameter Store (reuses the Miller browser), masked SecureStrings
4. Secrets Manager (list, describe, reveal with the secret-field widget)
5. DynamoDB (tables, Query-first explorer, bounded Scan)
6. RDS (merged cluster/instance tree, events, logs)
7. CloudFront (distributions, behaviours, origins)
8. EventBridge + Scheduler (rules, targets, pattern tester, next runs)
9. Cognito user pools and identity pools (users with server-side filter)
10. SES (account overview, identities, DNS helper)
11. Cost Explorer (overview + by-service, disk-cached, request counter)

Also: **C**- and **A**-type relations; multi-region "all regions" lists.

**Exit:** all six relation chains work. Every service in scope can be browsed
read-only.

## Phase 4: Actions

Rolled out by safety class, after read-only mode, prod marking and the confirm dialog
are in place:

1. **`side-effect`:** Lambda invoke, API GW test-invoke, SES test send, EventBridge
   test event (CloudFront function test is `safe`; it runs isolated)
2. **Mutating:**
   - EC2 start/stop/reboot
   - Lambda env edit, publish, alias
   - SSM put
   - Secrets put/rotate
   - alarm mute
   - rule/schedule enable/disable
   - Cognito user admin
   - CloudFront invalidation
   - RDS start/stop/snapshot
   - DynamoDB item edit
   - S3 upload/copy
   - IAM inline policy edit and attach/detach
3. **Destructive:** deletes, with typed confirmation and bulk preview.

Supporting pieces: the editor round trip with diff, optimistic concurrency
(`RevisionId`, `ETag`, condition expressions), and waiters / state polling after
actions.

**Exit:** every action shows its CLI command; DryRun/permission pre-checks are in
place where available; destructive actions can't run in read-only mode (covered by
tests).

## Phase 5: Power features

- EC2 SSM shell (`s`) and SSH; RDS connect (`C`), and SSM port-forwarding for private DBs
- Logs Insights with saved queries and the scan-cost warning
- SSM "compare environments" diff
- Saved views and bookmarks
- Cost hints in other views (Pricing API + cached CE data)
- In-TUI SSO login (device flow)
- EventBridge Pipes, Performance Insights, ECS / SQS / SNS (next service wave)

## Testing approach (all phases)

- **Unit:** ARN parser, filter → server-filter translation (Cognito, EC2, Logs), AWS
  cron next-run calculator, DynamoDB JSON ↔ plain JSON (with round-trip property
  tests), policy normalisation and URL-decoding.
- **Service integration:** each `svc-*` crate tests against recorded responses (the
  SDK's `aws-smithy-mocks` / replay client), so CI never needs AWS credentials.
- **UI snapshots:** `ratatui` `TestBackend` buffer snapshots (`insta`) for each main
  view, in both stale and error states.
- **Safety tests:** for every `ActionSpec`, assert that:
  - `Mutating` and `Destructive` actions are hidden in read-only mode
  - `SideEffect` actions always confirm, and are blocked on locked prod profiles
  - `Destructive` actions ask for typed confirmation
