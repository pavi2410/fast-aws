# EventBridge (+ Scheduler)

## Scope decision

| Part | Crate | v1? | Why |
|---|---|---|---|
| Event buses, rules, targets, archives | `aws-sdk-eventbridge` | **yes** | the core of the service |
| Scheduler (schedules, schedule groups) | `aws-sdk-scheduler` | **yes, read + enable/disable** | where most new cron jobs live now; users expect it next to rules |
| Pipes | `aws-sdk-pipes` | **no (later)** | less common; separate model |

Scope: regional.

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `eb:bus` | `eb`, `bus` | `ListEventBuses` | `DescribeEventBus` (with its policy) |
| `eb:rule` | `rule`, `rules` | `ListRules` (per bus) | `DescribeRule` + `ListTargetsByRule` |
| `eb:archive` | `archive` | `ListArchives` | `DescribeArchive` |
| `eb:replay` | (drill-in) | `ListReplays` | `DescribeReplay` |
| `sched:schedule` | `sched`, `schedule` | `ListSchedules` (per group) | `GetSchedule` |
| `sched:group` | (drill-in) | `ListScheduleGroups` | |

## Columns

- **Rules:** `NAME` · `BUS` · `STATE` · `TRIGGER` (`cron(…)` / `rate(…)` / `pattern: aws.ec2 …`) · `TARGETS` (count + first target, e.g. `λ thumbnailer +1`)
- **Schedules:** `NAME` · `GROUP` · `STATE` · `EXPRESSION` · `TZ` · `NEXT RUN` (calculated locally from the cron/rate expression) · `TARGET`

## Special views

- **Pattern viewer:** the event pattern pretty-printed, with a **pattern tester**
  (`T`). Paste or pick a sample event, then call `TestEventPattern` to see whether it
  matches.
- **Next runs:** for cron/rate rules and schedules, the next 5 run times in the
  selected time zone. This is computed locally; no API call.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `T` | Test pattern against an event | `TestEventPattern` | safe | `aws events test-event-pattern …` |
| `x→enable/disable` | Toggle a rule | `EnableRule` / `DisableRule` | mutating | `aws events disable-rule --name {n} --event-bus-name {b}` |
| `x→enable/disable` | Toggle a schedule | `GetSchedule` → `UpdateSchedule` with `State` changed | mutating | |
| `x→put` | Send a test event (`$EDITOR`, prefilled from the rule pattern) | `PutEvents` | side-effect | `aws events put-events --entries file://e.json` |
| `x→replay` | Start a replay from an archive (time range) | `StartReplay` | mutating | |
| `x→delete` | Delete rule (removes targets first) | `RemoveTargets` → `DeleteRule` | destructive | |

## Relations

- rule / schedule → targets: Lambda, SQS, SNS, Step Functions, another bus, API
  destination (only Lambda and buses can be followed in v1; others show their ARN)
- rule → the IAM role it uses to invoke targets
- target → dead-letter queue (ARN)
- bus → archives; archive → replays
- Lambda → rules and schedules that target it (reverse lookup: `ListRuleNamesByTarget`)

## IAM permissions

- **Read:** `events:List*`, `events:Describe*`, `events:TestEventPattern`, `scheduler:List*`, `scheduler:Get*`
- **Write:** `events:EnableRule`, `events:DisableRule`, `events:PutEvents`, `events:StartReplay`, `events:RemoveTargets`, `events:DeleteRule`, `scheduler:UpdateSchedule`

## Gotchas

- `ListRules` is per bus. List all buses first, then rules per bus; show the default
  bus first.
- Rules created by AWS services (`ManagedBy` set) can't be edited. Mark them and hide
  mutating actions for them.
- `UpdateSchedule` replaces the whole schedule. Send every field back exactly as read,
  changing only `State`.
- Cron syntax is AWS-specific (6 fields with year; `?` for day-of-month or
  day-of-week). The local next-run calculator must follow AWS rules, not Unix cron.
