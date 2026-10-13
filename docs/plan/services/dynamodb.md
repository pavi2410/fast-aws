# DynamoDB

## Crates & scope

- Crate: `aws-sdk-dynamodb`
- Scope: regional (global-table replicas shown as a column)

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `ddb:table` | `ddb`, `dynamo`, `table` | `ListTables` | `DescribeTable` + `DescribeContinuousBackups` (PITR) + `DescribeTimeToLive` |
| `ddb:item` | (drill-in) | `Query` / `Scan` (see below) | `GetItem` |
| `ddb:backup` | `ddb-backup` | `ListBackups` | `DescribeBackup` |

## Columns

- **Tables:** `NAME` · `PK` / `SK` · `BILLING` (on-demand / provisioned R/W) · `ITEMS≈` · `SIZE≈` · `GSIs` · `STREAM` · `PITR` · `CLASS`
- **Items:** the key attributes first, then the most common other attributes from the
  loaded page (worked out from the data). Use the column picker for more.

## Item explorer: Query first, Scan only when asked

```
 orders  PK=customerId (S)  SK=orderedAt (S)        index: [table] ▾      ● Query
 customerId = "C-1042"   orderedAt  begins_with  "2026-10"                [Enter] run
 customerId   orderedAt              status     total     items
▸C-1042       2026-10-12T09:13:44Z   SHIPPED    129.90    [3]
 C-1042       2026-10-03T17:01:02Z   PENDING     12.00    [1]
 page 1 · 2 items · 1.5 RCU consumed                      [n]ext page
```

- **Query mode (default):** a form built from the key schema of the table or the
  chosen GSI/LSI:
  - partition key: equals
  - sort key: an operator (`=`, `<`, `<=`, `>`, `>=`, `between`, `begins_with`)
  - optional filter expression
  - sends `ConsumedCapacity=TOTAL` and shows the RCU used after each page
- **Scan mode:** must be chosen explicitly (`S`).
  - A warning shows the table's approximate size.
  - Results come in bounded pages (`Limit`, default 50); `n` loads the next page.
    Never scan the whole table automatically.
- **PartiQL mode** (`:sql`): `ExecuteStatement` with a text input. `SELECT` without a
  key condition is a scan and gets the same warning.
- **Item view:** DynamoDB JSON (`{"S": "…"}`) is shown as plain JSON by default.
  Toggle with `J` to see the typed form.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `Enter` (item) | Item detail (plain JSON, foldable) | `GetItem` | safe | `aws dynamodb get-item --table-name {t} --key '{…}'` |
| `e` | Edit item as JSON → diff → put, with a condition that the item didn't change meanwhile | `PutItem` with `ConditionExpression` | mutating | |
| `n` | New item (template from the key schema) | `PutItem` with `attribute_not_exists(pk)` | mutating | |
| `D` | Export the current result set to JSON Lines / CSV | (already loaded + continue paging) | safe | |
| `Del` | Delete item(s) | `DeleteItem` / `BatchWriteItem` (25 per batch) | destructive | |
| `x→pitr` | Toggle point-in-time recovery | `UpdateContinuousBackups` | mutating | |
| `x→backup` | Create on-demand backup | `CreateBackup` | mutating | |
| `x→ttl` | Set the TTL attribute | `UpdateTimeToLive` | mutating | |

## Relations

- table → its stream → Lambda event source mappings
- table → GSIs / LSIs (picked in the item explorer)
- table → KMS key
- table → global-table replicas (`g` switches region and opens the same table)
- table → CloudWatch metrics (`ConsumedRead/WriteCapacityUnits`, `ThrottledRequests`,
  `SuccessfulRequestLatency`)
- table → backups

## IAM permissions

- **Read:** `dynamodb:ListTables`, `dynamodb:DescribeTable`, `dynamodb:DescribeContinuousBackups`, `dynamodb:DescribeTimeToLive`, `dynamodb:Query`, `dynamodb:Scan`, `dynamodb:GetItem`, `dynamodb:PartiQLSelect`, `dynamodb:ListBackups`
- **Write:** `dynamodb:PutItem`, `dynamodb:DeleteItem`, `dynamodb:BatchWriteItem`, `dynamodb:UpdateContinuousBackups`, `dynamodb:CreateBackup`, `dynamodb:UpdateTimeToLive`

## Gotchas

- **Scan uses read capacity.** On provisioned tables it can throttle production
  traffic; on on-demand tables it costs money. Hence Query-first and bounded,
  opt-in scans.
- `ItemCount` and `TableSizeBytes` are updated only about every 6 hours. Show them as
  approximate.
- Converting DynamoDB JSON to plain JSON loses type information: sets vs lists, and
  number precision. Editing works on the **typed** form, or keeps the original types
  (and refuses an edit that would change a set into a list). Keep numbers as strings
  internally, never `f64`.
- Item size limit is 400 KB. Validate before sending a put.
- A Query with a filter expression can return 0 items with a `LastEvaluatedKey`. Keep
  "next page" available and explain it.
