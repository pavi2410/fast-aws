# fast-aws: integration plan

A keyboard-driven TUI for AWS (think k9s for AWS). This directory is the plan for
integrating the first set of services. It is split into one file per topic so each
piece can be reviewed on its own.

## Reading order

1. [Foundation](00-foundation.md): architecture shared by every service
2. Services, one file each, all using the template below:
   - [EC2](services/ec2.md)
   - [S3](services/s3.md)
   - [Lambda](services/lambda.md)
   - [API Gateway (REST + HTTP/WebSocket)](services/api-gateway.md)
   - [CloudFront](services/cloudfront.md)
   - [CloudWatch Logs](services/cloudwatch-logs.md)
   - [CloudWatch Metrics & Alarms](services/cloudwatch-metrics.md)
   - [Cost Explorer](services/cost-explorer.md)
   - [IAM](services/iam.md)
   - [Cognito (User Pools + Identity Pools)](services/cognito.md)
   - [EventBridge (+ Scheduler)](services/eventbridge.md)
   - [SES](services/ses.md)
   - [Secrets Manager](services/secrets-manager.md)
   - [SSM Parameter Store](services/ssm-parameter-store.md)
   - [DynamoDB](services/dynamodb.md)
   - [RDS (+ Aurora)](services/rds.md)
3. [Cross-service relations](relations.md): the `g` (go-to) graph
4. [Phasing](phasing.md): build order and milestones

## Universal keymap

The same keys work for every resource type. Service docs only list *extra* keys.

| Key | Meaning |
|---|---|
| `:` / `Ctrl-P` | Command palette / fuzzy jump to a resource type (`:ec2`, `:s3`, `:fn`) |
| `/` | Filter the current list (text, or `tag=value`) |
| `Enter` | Drill into the selected item |
| `Esc` / `Backspace` | Go back |
| `d` | Describe: full detail (YAML/JSON viewer) |
| `l` | Logs for the selected item (if it has any) |
| `m` | Metrics for the selected item (if it has any) |
| `e` | Edit in `$EDITOR` (shows a diff before applying) |
| `y` / `Y` | Copy ID / copy ARN |
| `c` | Copy the equivalent `aws` CLI command for the last action or view |
| `x` | Action menu |
| `g` | Go to a related resource (see [relations](relations.md)) |
| `r` | Force refresh |
| `P` / `R` | Switch profile / region |
| `?` | Help for the current view |

## Service doc template

Each service file has these sections, in this order:

- **Crates & scope**: SDK crate(s) and whether it is regional, global, or pinned to one region
- **Resource kinds**: what appears in the palette, with the list/describe API calls
- **Columns**: default list columns
- **Actions**: key, API call, safety class (`safe` / `mutating` / `destructive`), CLI equivalent
- **Relations**: what `g` can jump to
- **Special views**: anything beyond table + describe
- **IAM permissions**: read-only set and the extra write set
- **Gotchas**: things that shape the design

Safety classes:

- `safe`: read-only, or an action with no lasting effect (such as copying a presigned URL)
- `mutating`: changes state but is reversible (stop an instance, disable a rule)
- `destructive`: deletes data or can't be undone; needs typed confirmation and is blocked in read-only mode
