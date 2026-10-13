# CloudWatch Metrics & Alarms

## Crates & scope

- Crate: `aws-sdk-cloudwatch`
- Scope: regional (CloudFront and some billing metrics live in `us-east-1`)

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `cw:alarm` | `alarms`, `alarm` | `DescribeAlarms` (metric + composite) | same + `DescribeAlarmHistory` |
| `cw:metric` | `metrics` | `ListMetrics` (namespace → metric → dimensions, as a browser) | `GetMetricData` |
| `cw:dashboard` | `dash` | `ListDashboards` | `GetDashboard` (rendered as a grid of charts) |

## Columns (alarms)

`STATE` (🔴 ALARM / 🟢 OK / ⚪ INSUFFICIENT) · `NAME` · `METRIC` (`ns/name dims`) · `THRESHOLD` · `UPDATED` · `ACTIONS` (on/off)

The default sort puts `ALARM` first. The header shows a count of alarms by state,
across all views.

## The metric widget (shared)

- **Sparkline** (one row, inside tables) and **chart** (full pane, braille line chart)
  from the same data.
- Period is picked from the time range (`1h`→60 s, `24h`→5 min, `7d`→1 h) so a request
  returns a sensible number of points.
- Other services describe their metrics in a small table, for example
  `Lambda: Errors, Invocations, Duration(p99), Throttles`, with the namespace and
  dimension templates. This widget uses those tables to fetch and draw them.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `Enter` | Chart the alarm's metric with the threshold line and alarm history | `GetMetricData`, `DescribeAlarmHistory` | safe | |
| `x→mute` | Disable/enable alarm actions | `DisableAlarmActions` / `EnableAlarmActions` | mutating | `aws cloudwatch disable-alarm-actions --alarm-names {n}` |
| `x→set-state` | Force a state (testing notifications) | `SetAlarmState` | mutating | |
| `x→delete` | Delete alarm | `DeleteAlarms` | destructive | |
| `t` | Change time range (`1h` / `3h` / `24h` / `7d` / custom) | — | safe | |

## Relations

- alarm → the resource its dimensions point to (`InstanceId` → EC2,
  `FunctionName` → Lambda, `DBInstanceIdentifier` → RDS, `TableName` → DynamoDB,
  `ApiName`/`ApiId` → API GW)
- alarm → SNS topics in its actions (shown by ARN; SNS itself is out of scope)
- From any resource, `m` opens its metrics view

## IAM permissions

- **Read:** `cloudwatch:DescribeAlarms`, `cloudwatch:DescribeAlarmHistory`, `cloudwatch:ListMetrics`, `cloudwatch:GetMetricData`, `cloudwatch:ListDashboards`, `cloudwatch:GetDashboard`
- **Write:** `cloudwatch:DisableAlarmActions`, `cloudwatch:EnableAlarmActions`, `cloudwatch:SetAlarmState`, `cloudwatch:DeleteAlarms`

## Gotchas

- **Cost:** `GetMetricData` is billed by the number of metrics requested. Sparklines
  in tables must:
  - fetch only the **visible rows**
  - batch up to 500 metric queries per call
  - be cached for about one period
  - never auto-refresh faster than 60 s
- `ListMetrics` only returns metrics with data in the last 2 weeks, and can be very
  large. Browse it by namespace instead of listing everything.
- Dashboard JSON can use metric math and search expressions. v1 renders simple
  metric widgets and shows a placeholder for anything else.
