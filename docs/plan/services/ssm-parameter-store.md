# SSM Parameter Store

## Crates & scope

- Crate: `aws-sdk-ssm` (shared with the EC2 session/agent features)
- Scope: regional

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `ssm:param` | `ssm`, `param`, `params` | `GetParametersByPath` (browser, one level at a time) or `DescribeParameters` (flat, with server-side filters) | `GetParameter` + `GetParameterHistory` + `ListTagsForResource` |

## Main view: the Miller-column browser (shared with S3)

```
 /                 │ prod/             │ payments/          │ /prod/payments/stripe_key
 ▸prod/            │ ▸payments/        │  db_host    String │ SecureString  v7  2026-10-01
  staging/         │  search/          │ ▸stripe_key Secure │ ••••••••••••  [v] reveal
  shared/          │  feature_flags    │  timeout_ms String │ KMS: alias/aws/ssm
```

- The browser component is the same one S3 uses, with a different data source:
  - the "folders" in a level are the distinct next path segments under the current
    path
  - the "files" are parameters at that level
- **Building one level:** `GetParametersByPath` with `Recursive=false` returns only
  the *leaf* parameters directly under a path, not sub-paths. Fetch the folder list
  from a single `DescribeParameters` scan of the prefix (`ParameterFilters`
  `Path`, `Option=Recursive`), cached, then split names into segments.
- Toggle a flat table with `B`:
  `NAME · TYPE · VERSION · TIER · LAST MODIFIED · MODIFIED BY`.
- Parameters without a leading `/` live at the root.

## Value handling

- `String` / `StringList`: values are shown straight away (`WithDecryption=false` is
  enough).
- `SecureString`: follows the [secret-handling rules](secrets-manager.md#secret-handling-rules).
  Masked; `v` fetches with `WithDecryption=true`; clipboard auto-clears; never cached
  to disk.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `v` | Reveal SecureString | `GetParameter` (`WithDecryption=true`) | safe* | `aws ssm get-parameter --name {n} --with-decryption` |
| `e` | Edit value (`$EDITOR`, diff) → new version | `PutParameter` (`Overwrite=true`) | mutating | `aws ssm put-parameter --name {n} --overwrite …` |
| `a` | Add parameter at the current path (form: name, type, value, tier, KMS) | `PutParameter` | mutating | |
| `H` | History: version table with a diff between any two versions | `GetParameterHistory` | safe* | |
| `x→label` | Attach a label to a version | `LabelParameterVersion` | mutating | |
| `x→export` | Export a subtree as `.env` / JSON / YAML (SecureStrings masked unless confirmed) | `GetParametersByPath` (recursive) | safe* | |
| `x→delete` | Delete (multi-select supported) | `DeleteParameters` (batches of 10) | destructive | |

\* Revealing SecureString values asks for confirmation on prod profiles.

## Special views

- **Compare environments:** pick two paths (e.g. `/staging/app/` and `/prod/app/`).
  Shows keys missing on either side and values that differ, with SecureStrings
  compared by hash and never displayed. This catches "works on staging, broken on
  prod" config drift.

## Relations

- parameter → KMS key (SecureString)
- reverse: Lambda env vars and EC2 user data that mention the parameter name
  (a best-effort text match, labelled as such)
- `ssm:/aws/reference/secretsmanager/…` parameters → the Secrets Manager secret

## IAM permissions

- **Read:** `ssm:DescribeParameters`, `ssm:GetParametersByPath`, `ssm:GetParameter`, `ssm:GetParameterHistory`, `ssm:ListTagsForResource`, plus `kms:Decrypt` to reveal
- **Write:** `ssm:PutParameter`, `ssm:LabelParameterVersion`, `ssm:DeleteParameters`

## Gotchas

- **Throttling is the main risk.** The default throughput for Parameter Store is low,
  and `GetParametersByPath` / `GetParameter` throttle quickly. Use a semaphore of
  about 2, adaptive retry, and cache tree levels aggressively.
- `PutParameter` with `Overwrite=true`: always send the original `Type`, `Tier` and
  `KeyId`. If `KeyId` is left out for a SecureString, it may be re-encrypted with the
  default `aws/ssm` key. Check this behaviour in the spike.
- `GetParameterHistory` on a SecureString returns values *decrypted* if asked. Pass
  `WithDecryption=false` unless the user is revealing.
- Advanced-tier parameters and policies (expiration) cost money. Show the tier, and
  don't upgrade it silently.
