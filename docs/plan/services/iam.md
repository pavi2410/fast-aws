# IAM

## Crates & scope

- Crate: `aws-sdk-iam`
- Scope: **global**. The region switcher is greyed out.

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `iam:role` | `role`, `roles` | `ListRoles` | `GetRole` + `ListAttachedRolePolicies` + `ListRolePolicies` (inline) + `ListInstanceProfilesForRole` |
| `iam:user` | `user`, `users` | `ListUsers` | `GetUser` + groups + attached/inline policies + `ListAccessKeys` + `GetAccessKeyLastUsed` + `ListMFADevices` |
| `iam:group` | `group` | `ListGroups` | `GetGroup` (members) + policies |
| `iam:policy` | `policy` | `ListPolicies` (`Scope=Local` by default; toggle `AWS`) | `GetPolicy` + `GetPolicyVersion` (default version) + `ListEntitiesForPolicy` |
| `iam:idp` | `idp` | `ListOpenIDConnectProviders`, `ListSAMLProviders` | |

## Columns

- **Roles:** `NAME` · `TRUSTED BY` (summarised: `lambda.amazonaws.com`, `account 1111…`, `OIDC github`) · `LAST USED` · `CREATED` · `PATH`
- **Users:** `NAME` · `MFA` · `KEYS` (count, oldest age) · `KEY LAST USED` · `CONSOLE`
- **Policies:** `NAME` · `ATTACHED` (count) · `DEFAULT VERSION` · `UPDATED`

## Special views

- **Effective permissions view:** for a role or user, all its policies (managed and
  inline) merged into one statement table: `EFFECT · ACTIONS · RESOURCES · CONDITION · SOURCE POLICY`.
  - Filter by action: `/s3:` shows only statements matching S3 actions, including
    wildcards.
  - This answers "what can this role actually do?" without opening each policy.
- **Trust policy** shown separately from permission policies, since it's usually the
  first thing to check.
- **"Can it?" simulator** (`S` key inside a role or user): enter an action, and
  optionally a resource ARN →
  `SimulatePrincipalPolicy` → allowed / denied, plus the statement that decided it.
- **Access-key hygiene:** a users view sorted by key age, with keys unused for 90 days
  highlighted.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `S` | Simulate action | `SimulatePrincipalPolicy` | safe | `aws iam simulate-principal-policy …` |
| `e` | Edit inline policy (JSON in `$EDITOR`, validated, diff) | `PutRolePolicy` / `PutUserPolicy` | mutating | |
| `x→attach` | Attach a managed policy (fuzzy-pick) | `AttachRolePolicy` / `AttachUserPolicy` | mutating | |
| `x→detach` | Detach a managed policy | `DetachRolePolicy` / `DetachUserPolicy` | mutating | |
| `x→key-deactivate` | Deactivate an access key | `UpdateAccessKey` (`Inactive`) | mutating | |
| `x→key-delete` | Delete an access key | `DeleteAccessKey` | destructive | |

Creating roles and users is out of scope for v1; that's better done in IaC.

## Relations

- role → services and principals that trust it
- role → resources that use it: Lambda functions, EC2 instance profiles, ECS tasks
  (later), Cognito identity pools. This is a reverse lookup across already-cached
  lists, so it shows "used by (from loaded views)", and `r` loads the rest.
- policy → attached roles, users and groups (`ListEntitiesForPolicy`)
- user → groups → policies

## IAM permissions

- **Read:** `iam:List*`, `iam:Get*`, `iam:SimulatePrincipalPolicy`
- **Write:** `iam:Put{Role,User}Policy`, `iam:Attach*/Detach*Policy`, `iam:UpdateAccessKey`, `iam:DeleteAccessKey`

## Gotchas

- **Policy documents are URL-encoded** in `GetRolePolicy`, `GetPolicyVersion`,
  `GetRole` (trust policy), etc. Decode them before parsing the JSON.
- `Action`, `Resource` and `Principal` can be a string *or* an array; normalise both.
- IAM has low API rate limits. Describing many roles fans out to 3–4 calls each, so
  load details lazily (when a role is selected or visible) and use a semaphore of
  about 4.
- `RoleLastUsed` is only filled in by `GetRole`, not by `ListRoles`. The
  `LAST USED` column loads lazily for visible rows.
- Editing a managed policy creates a new version, and the limit is 5. Out of scope for
  v1; only inline policies are editable.
