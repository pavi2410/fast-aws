# API Gateway

## Crates & scope

- Crates: **two separate APIs**
  - `aws-sdk-apigateway`: REST APIs (v1)
  - `aws-sdk-apigatewayv2`: HTTP and WebSocket APIs
- Scope: regional
- The TUI shows **one unified list**, with a `TYPE` column, so users don't need to know
  which API version an API uses.

## Resource kinds

| Kind | Aliases | List | Describe |
|---|---|---|---|
| `apigw:api` | `apigw`, `api` | v1 `GetRestApis` + v2 `GetApis`, merged | v1 `GetRestApi` / v2 `GetApi` |
| `apigw:route` | (drill-in) | v1 `GetResources` (with `embed=methods`) / v2 `GetRoutes` | v1 `GetMethod` + `GetIntegration` / v2 `GetRoute` + `GetIntegration` |
| `apigw:stage` | (drill-in) | v1 `GetStages` / v2 `GetStages` | |
| `apigw:deployment` | (drill-in) | v1 `GetDeployments` / v2 `GetDeployments` | |
| `apigw:authorizer` | (drill-in) | `GetAuthorizers` (both) | |
| `apigw:domain` | `domain` | v2 `GetDomainNames` (covers both types) + `GetApiMappings` | |
| `apigw:apikey` | `apikey` | v1 `GetApiKeys`, `GetUsagePlans` | |

## Columns

- **APIs:** `NAME` · `TYPE` (REST / HTTP / WS) · `ID` · `ENDPOINT` · `CREATED`
- **Routes:** `METHOD` · `PATH` · `INTEGRATION` (e.g. `λ order-processor`, `HTTP https://…`) · `AUTH`

## Actions

| Key | Action | API | Safety | CLI |
|---|---|---|---|---|
| `t` | Test-invoke a route (REST only; method, path, headers and body in `$EDITOR`) | v1 `TestInvokeMethod` | safe* | `aws apigateway test-invoke-method …` |
| `u` | Copy the invoke URL for the stage + route | — | safe | |
| `l` | Execution / access logs for the stage | Logs (via stage log settings) | safe | |
| `x→deploy` | Create a deployment to a stage | v1 `CreateDeployment` / v2 `CreateDeployment` | mutating | |
| `x→flush` | Flush stage cache (REST) | `FlushStageCache` | mutating | |
| `x→throttle` | Edit stage throttling | `UpdateStage` | mutating | |

\* `TestInvokeMethod` really calls the backend integration, so it can cause side
effects. Confirm it like Lambda invoke.

## Special views

**Route tree:** REST resources form a path tree (`/orders/{id}/items`); show it as a
tree. HTTP API routes are flat (`GET /orders/{id}`); show them as a sorted table.

## Relations

- route → integration target (Lambda function, HTTP URL, VPC link, AWS service)
- route → authorizer → Lambda authorizer function or Cognito user pool
- stage → access log group, execution log group (`API-Gateway-Execution-Logs_{apiId}/{stage}`)
- custom domain → API mappings → APIs; domain → ACM certificate
- API → CloudWatch metrics (`4XXError`, `5XXError`, `Latency`, `Count`)

## IAM permissions

- **Read:** `apigateway:GET` (on `arn:aws:apigateway:{region}::/*`)
- **Write:** `apigateway:POST`, `apigateway:PATCH` (deploy, update stage), `apigateway:DELETE` (cache flush)

## Gotchas

- API Gateway IAM permissions are HTTP-verb based (`apigateway:GET`), not named per
  API call. Show error messages in that form.
- REST integrations point to Lambda as a URI
  (`arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{fnArn}/invocations`).
  Parse the function ARN out of it to build the relation.
- `GetResources` without `embed=methods` needs one call per method. Always embed.
- Low default rate limits on the management API: keep the per-service semaphore
  around 2–4.
- WebSocket APIs: show `$connect`, `$disconnect` and `$default` routes like any others.
