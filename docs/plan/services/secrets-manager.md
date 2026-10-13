# Secrets Manager

## Crates & scope

- Crate: `aws-sdk-secretsmanager`
- Scope: regional (replicas are shown as a column)

## Secret-handling rules

These apply here and to SSM SecureString:

1. **Masked by default.** Values are never fetched just to show a list or a describe
   view. `GetSecretValue` is called only on an explicit key press.
2. **Reveal is temporary.** A revealed value is masked again when you leave the view
   or after `ui.reveal_secs`.
3. **Clipboard auto-clear.** Copying a value clears the clipboard after
   `ui.clipboard_clear_secs` (default 30 s), if the clipboard still holds the copied
   value.
4. **Never persisted:**
   - never in the disk cache
   - never in debug logs
   - never in the "copy CLI command" output (the command shows `--secret-string file://…`, not the value)
5. **Editor round trip uses a private temp file** (mode `0600`, in a `0700` dir), which
   is deleted when the edit finishes, including on panic (use a drop guard).

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `sm:secret` | `secrets`, `secret`, `sm` | `ListSecrets` (server-side filters: name, tag, description) | `DescribeSecret` (no value) |
| `sm:version` | (drill-in) | `ListSecretVersionIds` | |

## Columns

`NAME` · `DESCRIPTION` · `ROTATION` (on/off + last rotated) · `LAST CHANGED` · `LAST ACCESSED` · `KMS` · `REPLICAS`

Names with `/` (such as `prod/payments/stripe`) can also be browsed in the
Miller-column browser, like SSM paths. Toggle with `B`.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `v` | Reveal value (JSON secrets shown as a key/value table) | `GetSecretValue` | safe* | `aws secretsmanager get-secret-value --secret-id {id}` |
| `y` on a field | Copy one JSON key's value (auto-clear) | (already loaded) | safe* | |
| `e` | Edit value in `$EDITOR` → diff (masked) → new version | `PutSecretValue` | mutating | |
| `x→rotate` | Rotate now | `RotateSecret` | mutating | |
| `x→versions` | Versions and staging labels (`AWSCURRENT` / `AWSPREVIOUS`) | `ListSecretVersionIds` | safe | |
| `x→rollback` | Move `AWSCURRENT` to the previous version | `UpdateSecretVersionStage` | mutating | |
| `x→delete` | Schedule deletion (recovery window prompt, 7–30 days) | `DeleteSecret` | destructive | |
| `x→restore` | Cancel a scheduled deletion | `RestoreSecret` | mutating | |

\* Read-only, but revealing a value is sensitive: it's recorded in CloudTrail and
costs money. Prod profiles ask for confirmation first.

The diff for `e` shows which **keys** changed (`+ added`, `~ changed`, `- removed`),
not the values, unless you press `v` inside the diff.

## Relations

- secret → rotation Lambda
- secret → KMS key
- secret → the RDS instance or cluster that owns it (managed master-user secrets,
  `OwningService`)
- reverse: Lambda env vars and ECS task definitions (later) that reference the secret ARN

## IAM permissions

- **Read (no values):** `secretsmanager:ListSecrets`, `secretsmanager:DescribeSecret`, `secretsmanager:ListSecretVersionIds`
- **Reveal:** `secretsmanager:GetSecretValue`, plus `kms:Decrypt` on a customer-managed key
- **Write:** `secretsmanager:PutSecretValue`, `secretsmanager:RotateSecret`, `secretsmanager:UpdateSecretVersionStage`, `secretsmanager:DeleteSecret`, `secretsmanager:RestoreSecret`

## Gotchas

- `GetSecretValue` is billed per API call. Never call it for every row.
- Secrets scheduled for deletion are hidden by `ListSecrets` unless
  `IncludePlannedDeletion=true`. Show them in a toggle with a countdown.
- `SecretBinary` exists. Show its size and offer "save to file"; don't render it.
- Secrets owned by RDS can't be edited directly. Hide `e` and say why.
