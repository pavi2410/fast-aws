# SES

## Crates & scope

- Crate: `aws-sdk-sesv2`. Use the v2 API only; the legacy `ses` API isn't needed for
  anything in scope.
- Scope: regional. Identities, quotas and sandbox status are all **per region**, so
  the header shows the region prominently.

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `ses:account` | `ses` | — (single object) | `GetAccount` (sandbox, quota, send rate, enforcement status) |
| `ses:identity` | `identity` | `ListEmailIdentities` | `GetEmailIdentity` (verification, DKIM, MAIL FROM, policies) |
| `ses:configset` | `configset` | `ListConfigurationSets` | `GetConfigurationSet` + `GetConfigurationSetEventDestinations` |
| `ses:template` | `template` | `ListEmailTemplates` | `GetEmailTemplate` |
| `ses:suppression` | `suppressed` | `ListSuppressedDestinations` | `GetSuppressedDestination` |

## Main view: account overview

```
 SES — eu-west-1          ⚠ SANDBOX            Sending: ENABLED   Enforcement: HEALTHY
 Quota (24h): 1,240 / 50,000  ██▍                 Max rate: 14/s
 IDENTITIES                  TYPE     STATUS     DKIM        MAIL FROM
▸example.com                 DOMAIN   SUCCESS    SUCCESS     bounce.example.com ✓
 alerts@example.org          EMAIL    PENDING    —           —
```

- The sandbox warning is prominent, because "why aren't my emails arriving" is the
  most common SES question.
- **DNS helper** for a domain identity (`Enter`): the exact DKIM CNAME, MAIL FROM MX
  and SPF TXT records to create, each copyable.

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `t` | Send a test email (from a verified identity; form: to, subject, body; optional template + data) | `SendEmail` | mutating | `aws sesv2 send-email …` |
| `x→render` | Render a template with test data | `TestRenderEmailTemplate` | safe | |
| `x→verify` | Add an identity (email or domain) | `CreateEmailIdentity` | mutating | |
| `x→unsuppress` | Remove an address from the suppression list | `DeleteSuppressedDestination` | mutating | |
| `e` | Edit template (`$EDITOR`: subject, HTML, text) | `UpdateEmailTemplate` | mutating | |
| `x→delete` | Delete identity | `DeleteEmailIdentity` | destructive | |

## Relations

- config set → event destinations: CloudWatch, SNS, EventBridge, Firehose
  (bus and metrics can be followed with `g`)
- identity → Cognito user pools that use it for email
- identity → Lambda / SNS that receive bounces and complaints (via config set)
- account → CloudWatch metrics (`Send`, `Bounce`, `Complaint`, `Reputation.BounceRate`)

## IAM permissions

- **Read:** `ses:GetAccount`, `ses:ListEmailIdentities`, `ses:GetEmailIdentity`, `ses:ListConfigurationSets`, `ses:GetConfigurationSet*`, `ses:ListEmailTemplates`, `ses:GetEmailTemplate`, `ses:ListSuppressedDestinations`
- **Write:** `ses:SendEmail`, `ses:CreateEmailIdentity`, `ses:UpdateEmailTemplate`, `ses:DeleteSuppressedDestination`, `ses:DeleteEmailIdentity`

## Gotchas

- IAM action names use the `ses:` prefix for v2 too; there's no separate `sesv2:`
  prefix.
- In the sandbox, test sends only reach verified recipients. Check this before
  sending, and explain it instead of showing a raw `MessageRejected`.
- The suppression list can be large; page it on the server and filter by reason
  (BOUNCE / COMPLAINT).
- Receipt rules (inbound email) are only in the legacy API and are out of scope.
