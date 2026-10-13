# CloudFront

## Crates & scope

- Crates: `aws-sdk-cloudfront`; `aws-sdk-acm` (pinned to `us-east-1`) for certificate details
- Scope: **global**. Build the client with region `us-east-1`. The region switcher is
  greyed out in these views.

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `cf:distribution` | `cf`, `cloudfront`, `dist` | `ListDistributions` | `GetDistribution` (config + status + ETag) |
| `cf:invalidation` | (drill-in) | `ListInvalidations` | `GetInvalidation` |
| `cf:function` | `cff` | `ListFunctions` | `DescribeFunction`, `GetFunction` (code) |
| `cf:cache-policy` | (drill-in / `g`) | `ListCachePolicies` (`Type=custom` by default) | `GetCachePolicy` |
| `cf:oac` | (drill-in / `g`) | `ListOriginAccessControls` | |

## Columns

**Distributions:** `ID` · `ALIASES` (CNAMEs, first one + `+N`) · `DOMAIN` (`dxxxx.cloudfront.net`) · `STATUS` (Deployed / InProgress) · `ENABLED` · `ORIGINS` (count) · `PRICE CLASS`

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `I` | Invalidate paths (prompt, default `/*`) | `CreateInvalidation` | mutating | `aws cloudfront create-invalidation --distribution-id {id} --paths "/*"` |
| `x→enable/disable` | Toggle enabled | `GetDistributionConfig` → `UpdateDistribution` (`IfMatch` ETag) | mutating | |
| `x→test-fn` | Test a CloudFront Function with an event in `$EDITOR` | `TestFunction` | safe | |
| `u` | Copy the distribution URL | — | safe | |

## Special views

- **Behaviours table:** path pattern → origin → cache policy → viewer protocol →
  function/Lambda@Edge associations. This is where most "why is this cached" questions
  get answered.
- **Origins table:** origin ID, domain, type (S3 / custom / ALB), OAC/OAI.
- **Invalidations:** list with status. A new invalidation is polled until
  `Completed` and shown in the status line.

## Relations

- distribution → origins → S3 bucket / ALB / API Gateway / custom host
- distribution → ACM certificate (`us-east-1`)
- distribution → cache, origin-request and response-headers policies
- distribution → CloudFront Functions and Lambda@Edge functions (in `us-east-1`)
- distribution → WAF web ACL (shows the ARN; WAF itself is out of scope)
- distribution → access log bucket
- distribution → CloudWatch metrics (in `us-east-1`)

## IAM permissions

- **Read:** `cloudfront:List*`, `cloudfront:Get*`, `cloudfront:DescribeFunction`, `acm:DescribeCertificate`
- **Write:** `cloudfront:CreateInvalidation`, `cloudfront:UpdateDistribution`, `cloudfront:TestFunction`

## Gotchas

- Every update needs the current `ETag` as `IfMatch`, and takes the *full* config.
  Use the same read → edit → diff → write cycle as everything else, and treat a
  `PreconditionFailed` error as "changed meanwhile, reload".
- Deployment takes minutes (`InProgress` → `Deployed`). Poll slowly (every 30 s) and
  only while the view is open.
- Lambda@Edge functions live in `us-east-1` regardless of the current region, so the
  relation needs a client for that region.
- CloudFront metrics are in `us-east-1` with dimension `Region=Global`.
- Invalidations are free for the first 1,000 paths a month, then charged per path.
  Show this note in the invalidate dialog.
