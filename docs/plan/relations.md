# Cross-service relations (the `g` graph)

`g` on any row opens a menu of related resources and jumps there with the right
profile, region and filter already set. This is the main thing the console can't do
quickly, so relations are first-class data in each `ResourceKind::relations()`.

## How a relation is resolved

| Method | Meaning | Cost |
|---|---|---|
| **F** forward field | the ID/ARN is on the source object (e.g. Lambda `Role`) | free |
| **A** reverse via API | a dedicated "who uses X" call exists (e.g. `ListEntitiesForPolicy`) | 1 call |
| **C** reverse via cache | scan already-loaded lists of the target kind (e.g. "Lambdas using this role") | free, may be partial; shown as "from loaded views", and `r` loads the rest |
| **N** name inference | derived from a naming convention (e.g. `/aws/lambda/{fn}`) | free, best effort; labelled "inferred" |

ARNs are parsed into `(service, region, account, resource)` by one shared parser.
Jumping to an ARN in another region switches the region for that view only. Jumping
to an ARN in another **account** is shown, but isn't a link unless a configured
profile matches that account ID.

## Matrix

Rows are the source, the target is in the cell, and the method is in brackets.

| From \ To | EC2 | S3 | Lambda | API GW | CloudFront | Logs | Metrics | IAM | Cognito | EventBridge | SES | Secrets | SSM | DynamoDB | RDS |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **EC2** | SG, subnet, VPC, vol, AMI [F] | | | | | console output | CPU, net [F] | instance profile → role [F] | | | | | managed instance [A] | | |
| **S3** | | | notifications [A] | | distributions using bucket [C] | | size metrics [F] | | | | | | | | |
| **Lambda** | VPC, subnets, SGs [F] | triggers [C] | layers, versions, aliases [F] | routes integrating [C] | Lambda@Edge use [C] | log group [F] | errors, duration [F] | exec role [F] | triggers on pools [C] | rules/schedules targeting [A] | | refs in env [C] | refs in env [C] | stream ESM [F] | |
| **API GW** | | | integration target, authorizer [F] | stages, domains [F] | origin pointing here [C] | access/exec logs [F/N] | 4xx/5xx/latency [F] | invoke role [F] | authorizer user pool [F] | | | | | | |
| **CloudFront** | | S3 origins [F] | Lambda@Edge [F] | API origins [F] | policies, functions [F] | | requests/errors [F] | | | | | | | | |
| **Logs** | | | source fn [N] | source stage [N] | | streams [F] | | | | | | | | | source DB [N] |
| **Metrics** (alarm) | by dimension [F] | by dimension [F] | by dimension [F] | by dimension [F] | by dimension [F] | | metric [F] | | | | | | | by dimension [F] | by dimension [F] |
| **IAM** (role) | instances using [C] | | functions using [C] | | | | | attached policies [F], policy → entities [A] | identity pools using [C] | rules using [C] | | | | | |
| **Cognito** | | | triggers [F] | authorizers using [C] | | | | identity-pool roles [F] | user pool ↔ identity pool [F] | | email identity [F] | client secret (inline) | | | |
| **EventBridge** | | | targets [F] | | | | invocations [F] | target role [F] | | buses, archives [F] | | | | | |
| **SES** | | | | | | | send/bounce [F] | | user pools using [C] | event destination [F] | | | | | |
| **Secrets** | | | rotation fn [F] | | | | | | | | | versions [F] | `/aws/reference/…` [N] | | owning DB [F] |
| **SSM** | | | | | | | | | | | | referenced secret [N] | path siblings | | |
| **DynamoDB** | | | stream consumers [A] | | | | capacity, throttles [F] | | | | | | | GSIs, replicas, backups [F] | |
| **Cost Explorer** | `:ec2` [F] | `:s3` [F] | `:fn` [F] | `:apigw` [F] | `:cf` [F] | `:logs` [F] | `:metrics` [F] | | `:userpool` [F] | `:eb` [F] | `:ses` [F] | `:secrets` [F] | `:ssm` [F] | `:ddb` [F] | `:rds` [F] |
| **RDS** | VPC, SGs [F] | | | | | log exports [N] | CPU, conns [F] | | | | | master secret [F] | | | cluster ↔ instances, snapshots [F] |

(Cost Explorer rows jump from a service's spend to that service's resource list. IAM
has no cost line, so it has no link. Tag-group → resources comes later; see
[Cost Explorer](services/cost-explorer.md#relations).)

(KMS keys appear on many rows as a forward ARN. KMS isn't an integrated service in
v1, so these show the key ARN/alias without a jump.)

## Most valuable chains (used as acceptance tests in phasing)

1. **"Why is this endpoint 500ing?"**: API GW route → Lambda → log pane, filtered to `ERROR`.
2. **"What can this function touch?"**: Lambda → role → effective permissions view.
3. **"Who sends traffic to this bucket?"**: S3 bucket → CloudFront distribution → behaviours.
4. **"Is this alarm real?"**: Alarm → resource → metrics chart + logs.
5. **"What's this DB's password?"**: RDS cluster → master secret → reveal (with confirmation).
6. **"What runs on a schedule?"**: EventBridge rule / schedule → Lambda → recent invocations.

## Implementation notes

- `RelationRef { label, target_kind, target: Target }`, where `Target` is one of:
  - `Id`
  - `Arn`
  - `Filter`: a list query on the target kind, e.g. `:fn /role=arn:…`
  - `Lazy`: an async resolver for kinds A and C
- **C**-type relations read from the shared cache, so they don't add API calls. This
  is why the cache key includes account and region.
- The `g` menu shows the resolution method as a dim tag (`inferred`, `partial`), so
  users know how far to trust it.
