# Cognito (User Pools + Identity Pools)

## Crates & scope

- Crates: **two separate APIs**
  - `aws-sdk-cognitoidentityprovider`: **User Pools** (users, groups, app clients)
  - `aws-sdk-cognitoidentity`: **Identity Pools** (federated identities → AWS credentials)
- Scope: regional
- They are separate palette entries (`:userpool`, `:idpool`), linked with `g`.

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `cognito:userpool` | `userpool`, `up` | `ListUserPools` (`MaxResults` is required) | `DescribeUserPool` |
| `cognito:user` | (drill-in) | `ListUsers` (server-side `Filter`) | `AdminGetUser` + `AdminListGroupsForUser` |
| `cognito:group` | (drill-in) | `ListGroups` | `ListUsersInGroup` |
| `cognito:client` | (drill-in) | `ListUserPoolClients` | `DescribeUserPoolClient` |
| `cognito:idp` | (drill-in) | `ListIdentityProviders` | `DescribeIdentityProvider` |
| `cognito:idpool` | `idpool`, `ip` | `ListIdentityPools` | `DescribeIdentityPool` + `GetIdentityPoolRoles` |

## Columns

- **User pools:** `NAME` · `ID` · `USERS` (`EstimatedNumberOfUsers` from describe, lazy) · `MFA` · `CREATED`
- **Users:** `USERNAME` · `EMAIL` · `STATUS` (CONFIRMED / FORCE_CHANGE_PASSWORD / …) · `ENABLED` · `MFA` · `CREATED`
- **Identity pools:** `NAME` · `ID` · `UNAUTH ALLOWED` · `AUTH ROLE` · `UNAUTH ROLE`

## User search

`/` in the users view turns into a Cognito filter expression on the server:

| You type | Filter sent |
|---|---|
| `/alice@` | `email ^= "alice@"` |
| `/sub=…` | `sub = "…"` |
| `/status=FORCE_CHANGE_PASSWORD` | `cognito:user_status = "FORCE_CHANGE_PASSWORD"` |

The attribute is chosen from a small picker (email, phone, username, sub, status).
Only one filter attribute is allowed per request; that's an API limit.

## Actions (users)

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `x→disable/enable` | Disable / enable user | `AdminDisableUser` / `AdminEnableUser` | mutating | `aws cognito-idp admin-disable-user --user-pool-id {p} --username {u}` |
| `x→reset` | Reset password | `AdminResetUserPassword` | mutating | |
| `x→set-password` | Set password (permanent or temporary) | `AdminSetUserPassword` | mutating | |
| `x→confirm` | Confirm sign-up | `AdminConfirmSignUp` | mutating | |
| `x→groups` | Add to / remove from group | `AdminAddUserToGroup` / `AdminRemoveUserFromGroup` | mutating | |
| `e` | Edit attributes (YAML) | `AdminUpdateUserAttributes` | mutating | |
| `x→signout` | Global sign-out | `AdminUserGlobalSignOut` | mutating | |
| `x→delete` | Delete user | `AdminDeleteUser` | destructive | |
| `x→create` | Create user (form) | `AdminCreateUser` | mutating | |

App clients: show the client secret with the secret-field widget (masked; reveal on a
key press). Copy the hosted-UI / OAuth endpoints with `u`.

## Relations

- user pool → Lambda triggers (pre sign-up, post confirmation, pre token generation, …)
- user pool → app clients → identity pools that use them
- user pool → API Gateway authorizers that reference it
- identity pool → auth/unauth IAM roles, plus role-mapping rules
- identity pool → user pools and app clients configured as providers
- user pool → SES identity used for email (when `EmailSendingAccount=DEVELOPER`)

## IAM permissions

- **Read:** `cognito-idp:ListUserPools`, `cognito-idp:DescribeUserPool`, `cognito-idp:ListUsers`, `cognito-idp:AdminGetUser`, `cognito-idp:AdminListGroupsForUser`, `cognito-idp:ListGroups`, `cognito-idp:ListUsersInGroup`, `cognito-idp:ListUserPoolClients`, `cognito-idp:DescribeUserPoolClient`, `cognito-identity:ListIdentityPools`, `cognito-identity:DescribeIdentityPool`, `cognito-identity:GetIdentityPoolRoles`
- **Write:** the `cognito-idp:Admin*` actions listed above

## Gotchas

- `ListUsers` has **low rate limits**, and pools can have millions of users. Never
  load all users. Show the first page or two, then rely on server-side filtering;
  the semaphore for this service is about 2.
- `ListUsers` filtering supports only standard attributes, `=` and `^=`, and one
  attribute per request. Make that clear in the filter UI.
- Usernames may be UUIDs when email is an alias, so show `email` beside them.
- User data is personal data. Never put it in logs or on-disk caches; keep it in
  memory only.
