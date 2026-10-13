# CloudWatch Logs

The log pane built here is reused by Lambda, API Gateway, RDS, ECS (later) and EC2
console output, so this service comes early in the [phasing](../phasing.md).

## Crates & scope

- Crate: `aws-sdk-cloudwatchlogs`
- Scope: regional

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `logs:group` | `logs`, `lg` | `DescribeLogGroups` (prefix / pattern filter goes to the server) | same + `ListTagsForResource` |
| `logs:stream` | (drill-in) | `DescribeLogStreams` (`orderBy=LastEventTime`, descending) | |
| `logs:query` | `insights` | saved queries: `DescribeQueryDefinitions`; history: `DescribeQueries` | |

## Columns

- **Groups:** `NAME` · `RETENTION` · `STORED` (bytes) · `CLASS` (Standard / IA) · `KMS`
- **Streams:** `NAME` · `LAST EVENT` · `FIRST EVENT`

## The log pane (shared widget)

```
 /aws/lambda/order-processor   ● LIVE   filter: "ERROR"   since: 15m      [w]rap [t]ime [j]son
 10:42:01.112  START RequestId: 7f1c…
 10:42:01.340  ERROR  {"msg":"payment declined","orderId":"A-991"}
 10:42:01.351  END RequestId: 7f1c…  Duration: 239 ms
```

- **Modes:**
  - **Live**: `StartLiveTail`, an event stream.
  - **Range**: `FilterLogEvents` with a start/end time, paginated backwards as you
    scroll up.
  - **Stream**: `GetLogEvents` for a single stream.
- **Live-tail fallback:** if the `StartLiveTail` spike fails, or the account hits its
  concurrent-session limit, fall back to polling `FilterLogEvents` every 2 s with
  `startTime` = last seen timestamp, de-duplicating by `eventId`.
- Filters use CloudWatch filter-pattern syntax, sent to the server. A separate
  local highlight (`n`/`N` to jump) doesn't call the API again.
- JSON log lines can be pretty-printed and folded with `j`.
- Time jumps: `-15m`, `-1h`, `2026-10-13 09:00`, or a relative range.
- `Y` copies the line; `y` copies the request ID if one is found.

## Logs Insights view

- Write the query in a text box or in `$EDITOR`, with saved queries as presets.
- Pick log groups with multi-select from the group list.
- `StartQuery` → poll `GetQueryResults` every 1 s → show the results table as it fills
  in → `StopQuery` if the view closes.
- Show records and bytes scanned. Insights is **billed per GB scanned**, so show the
  time range clearly and warn on large ranges over many groups.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `l` / `Enter` | Open log pane (live) | `StartLiveTail` | safe | `aws logs tail {group} --follow` |
| `I` | Insights query on selected groups | `StartQuery` | safe ($) | `aws logs start-query …` |
| `x→retention` | Set retention | `PutRetentionPolicy` | mutating | `aws logs put-retention-policy --log-group-name {g} --retention-in-days N` |
| `D` | Export the visible range to a local file | `FilterLogEvents` | safe | |
| `x→delete` | Delete log group | `DeleteLogGroup` | destructive | |

## Relations

log group → source (inferred from the name):

| Name pattern | Source |
|---|---|
| `/aws/lambda/X` | Lambda X |
| `API-Gateway-Execution-Logs_{id}/{stage}` | API Gateway stage |
| `/aws/rds/instance/X/…` | RDS instance |

Also: log group → KMS key, log group → subscription filters (`DescribeSubscriptionFilters`)
→ their destination Lambda or Kinesis stream.

## IAM permissions

- **Read:** `logs:DescribeLogGroups`, `logs:DescribeLogStreams`, `logs:FilterLogEvents`, `logs:GetLogEvents`, `logs:StartLiveTail`, `logs:StartQuery`, `logs:GetQueryResults`, `logs:StopQuery`, `logs:DescribeQueryDefinitions`
- **Write:** `logs:PutRetentionPolicy`, `logs:DeleteLogGroup`

## Gotchas

- Live Tail is billed by session minutes, and accounts have a concurrent-session
  limit. Close the session as soon as the pane closes, and pause it when the TUI
  loses focus for a long time.
- `FilterLogEvents` over many streams can return empty pages with a `nextToken`.
  Keep paginating, and show "searching…" while it does.
- `DescribeLogGroups` is lightly rate-limited. Cache it, and search by prefix on the
  server instead of loading thousands of groups.
