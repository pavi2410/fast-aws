# Lambda

## Crates & scope

- Crates: `aws-sdk-lambda`; uses `cloudwatchlogs` and `cloudwatch` for logs and metrics
- Scope: regional

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `lambda:function` | `lambda`, `fn` | `ListFunctions` | `GetFunction` (config + code location + tags) |
| `lambda:version` | (drill-in) | `ListVersionsByFunction` | |
| `lambda:alias` | (drill-in) | `ListAliases` | `GetAlias` |
| `lambda:esm` | `esm` | `ListEventSourceMappings` | `GetEventSourceMapping` |
| `lambda:layer` | `layer` | `ListLayers` | `ListLayerVersions` |

## Columns (functions)

`NAME` · `RUNTIME` · `MEM` · `TIMEOUT` · `ERRORS 24h` (sparkline + count) · `INVOCATIONS 24h` · `LAST DEPLOY`

- Fetch the metric columns only for visible rows, in one batched `GetMetricData`
  call (see [CloudWatch metrics](cloudwatch-metrics.md) on cost).
- Sorting by errors puts broken functions at the top.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `i` | Invoke (payload in `$EDITOR`) | `Invoke` with `LogType=Tail` | mutating* | `aws lambda invoke --function-name {name} --payload file://p.json out.json` |
| `l` | Tail logs (`/aws/lambda/{name}`, or the configured log group) | Logs live tail | safe | `aws logs tail /aws/lambda/{name} --follow` |
| `e` | Edit env vars as YAML → diff → apply | `UpdateFunctionConfiguration` | mutating | |
| `x→concurrency` | Set/clear reserved concurrency | `PutFunctionConcurrency` / `DeleteFunctionConcurrency` | mutating | |
| `x→publish` | Publish version | `PublishVersion` | mutating | |
| `x→alias` | Point an alias at a version (with weighted routing) | `UpdateAlias` | mutating | |
| `x→deploy` | Upload a zip from a local path | `UpdateFunctionCode` | mutating | |
| `x→delete` | Delete function | `DeleteFunction` | destructive | |

\* Invoke is classed as mutating because the function can have side effects. It's
allowed in read-only mode only after an extra confirmation.

## Special views

- **Invoke console:** a split pane.
  - Left: the payload, chosen from saved test events stored per function in
    `~/.config/fast-aws/events/`.
  - Right: the response, status, duration, billed duration, and the log tail
    (from the decoded base64 `LogResult`).
- **Config panel:** env vars (masked if the key looks like a secret), VPC, layers,
  role, the resource policy (`GetPolicy`), and triggers.

## Relations

- function → execution role (IAM)
- function → log group
- function → event source mappings → SQS / DynamoDB stream / Kinesis
- function → API Gateway routes that integrate with it
- function → EventBridge rules targeting it
- function → S3 bucket notifications
- function → Cognito triggers
- function → VPC, subnets and security groups
- function → layers

## IAM permissions

- **Read:** `lambda:List*`, `lambda:Get*`, `logs:*` read actions (see logs), `cloudwatch:GetMetricData`
- **Write:** `lambda:InvokeFunction`, `lambda:UpdateFunctionConfiguration`, `lambda:UpdateFunctionCode`, `lambda:PublishVersion`, `lambda:UpdateAlias`, `lambda:PutFunctionConcurrency`, `lambda:DeleteFunction`

## Gotchas

- Lambda ignores updates while a previous update is still in progress
  (`LastUpdateStatus=InProgress`). Wait using the `function_updated` waiter before
  allowing the next edit.
- Editing env vars replaces the *whole* `Environment` map. Always start from a fresh
  read, and fail if the function changed meanwhile (compare `RevisionId`).
- The `Invoke` log tail is capped at the last 4 KB. For more, open the full log pane.
- Functions can log to a custom log group (`LoggingConfig.LogGroup`). Don't assume
  `/aws/lambda/{name}`.
