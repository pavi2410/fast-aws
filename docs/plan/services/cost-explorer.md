# Cost Explorer

## Crates & scope

- Crate: `aws-sdk-costexplorer`
- Scope: **pinned to `us-east-1`** (the Cost Explorer endpoint), whatever the
  current region.

## ⚠ Each API request costs money

Cost Explorer charges **per API request** ($0.01 each at the time of writing). The
whole design follows from this:

- **Never auto-refresh.** Data is fetched on open (if the cache is stale) and on `r`.
- **Disk cache** with a long TTL (default 6 h; the cost data itself updates only a few
  times a day). Keyed by `(account, query)`, so returning to a view is free.
- **Request counter** in the view header: `CE requests this session: 4 (~$0.04)`.
- Each view needs **one request** where possible: group-bys are designed so a single
  `GetCostAndUsage` call fills the whole view.

## Views (not a typical resource list)

| View | Alias | API | Notes |
|---|---|---|---|
| Overview | `cost` | `GetCostAndUsage` (month-to-date, daily, `UnblendedCost`) + `GetCostForecast` | MTD total, forecast, the same period last month, and a daily bar chart |
| By service | `cost/svc` | `GetCostAndUsage` grouped by `SERVICE` | table + share bars; `Enter` → that service by `USAGE_TYPE` |
| By account | `cost/acct` | grouped by `LINKED_ACCOUNT` | only useful in a management account |
| By tag | `cost/tag` | grouped by `TAG:{key}` (key picked from `GetTags`) | "untagged" spend shown explicitly |
| Anomalies | `cost/anom` | `GetAnomalies` | most recent first, with impact $ and root causes |

```
 Cost — 123456789012   Oct 2026 MTD  $1,284.10   forecast $3,020   last month $2,870   [CE reqs: 2]
 ▁▂▂▃▂▂▃▃▂▂▃▅▃                                                          daily, last 13 days
 SERVICE                       MTD        SHARE
 Amazon EC2                    $612.40    ██████████▌        48%
 Amazon RDS                    $301.22    █████▏             23%
 AWS Lambda                    $ 41.07    ▋                   3%
```

## Actions

| Key | Action | Safety |
|---|---|---|
| `t` | Change range (MTD / last month / 3 months / custom) and granularity | safe ($: new query) |
| `M` | Change metric (Unblended / Amortized / Net) | safe ($) |
| `F` | Filter by dimension (service, region, account, tag) | safe ($) |
| `c` | Copy the equivalent `aws ce get-cost-and-usage …` | safe |
| `E` | Export the current table to CSV | safe |

Before an action that triggers a new query, the status line shows
`will make 1 request ($0.01)`.

## Cost hints in other views (later phase)

Show estimated cost inline in other views, e.g. an EC2 instance's hourly price or a
service's MTD in a header. These hints read from the **cached** by-service breakdown,
and use the Pricing API (`aws-sdk-pricing`, disk-cached catalogue) for unit prices. They
never trigger Cost Explorer calls.

## Relations

- service row → that service's resource list in the TUI (EC2 → `:ec2`, etc.)
- tag group → resources with that tag, through the
  `resourcegroupstaggingapi` crate, which is optional and later

## IAM permissions

- **Read:** `ce:GetCostAndUsage`, `ce:GetCostForecast`, `ce:GetDimensionValues`, `ce:GetTags`, `ce:GetAnomalies`
- No write actions in scope.

## Gotchas

- Cost Explorer must be enabled for the account. Detect the "not enabled" error and
  explain it.
- The current day's data is partial, and the last few days can change. Mark them as
  estimates.
- Forecast fails when there isn't enough history. Show "no forecast" instead of an error.
- Member accounts in an Organization may be denied access by the management account
  (billing access). Show the AccessDenied in the pane.
- Resource-level data (`GetCostAndUsageWithResources`) needs opt-in and covers only
  14 days. It's out of scope for v1.
