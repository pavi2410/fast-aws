//! `apigw:route`: an API's routes (REST resources × methods, or v2 route keys),
//! each with what it integrates with. Only reachable by drilling into an API.

use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_apigateway::Client as RestClient;
use aws_sdk_apigatewayv2::Client as V2Client;
use fast_aws_core::fmt::or_dash;
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, kind_info,
};
use serde_json::json;

use crate::{Flavor, integration_target, split_id};

pub struct Routes;

const COLUMNS: &[Column] = &[
    Column::fixed("METHOD", 7),
    Column::fill("PATH", 3),
    Column::fill("INTEGRATION", 3),
    Column::fixed("AUTH", 14),
];

#[async_trait]
impl ResourceKind for Routes {
    kind_info! {
        id: "apigw:route",
        title: "API Routes",
        aliases: ["routes"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let api = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("API"))?;
        match split_id(&api.id) {
            (Flavor::Rest, id) => {
                list_rest(&RestClient::new(ctx.config()), id, query.next.clone()).await
            }
            (Flavor::V2, id) => list_v2(&V2Client::new(ctx.config()), id, query.next.clone()).await,
        }
    }

    /// The row has everything listed; describing re-reads the single route.
    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        id: &str,
    ) -> Result<Detail, AwsError> {
        let api = parent.ok_or_else(|| AwsError::not_found("API"))?;
        match split_id(&api.id) {
            (Flavor::Rest, api_id) => {
                let (resource_id, method) = id.split_once(' ').unwrap_or((id, "ANY"));
                let m = RestClient::new(ctx.config())
                    .get_method()
                    .rest_api_id(api_id)
                    .resource_id(resource_id)
                    .http_method(method)
                    .send()
                    .await
                    .map_err(AwsError::from_sdk)?;
                let i = m.method_integration();
                let fields = json!({
                    "HttpMethod": m.http_method(),
                    "AuthorizationType": m.authorization_type(),
                    "AuthorizerId": m.authorizer_id(),
                    "ApiKeyRequired": m.api_key_required(),
                    "Integration": i.map(|i| json!({
                        "Type": i.r#type().map(|t| t.as_str()),
                        "Uri": i.uri(),
                        "HttpMethod": i.http_method(),
                        "TimeoutInMillis": i.timeout_in_millis(),
                        "ConnectionType": i.connection_type().map(|c| c.as_str()),
                    })),
                });
                Ok(Detail {
                    fields,
                    raw: format!("{m:#?}"),
                })
            }
            (Flavor::V2, api_id) => {
                let r = V2Client::new(ctx.config())
                    .get_route()
                    .api_id(api_id)
                    .route_id(id)
                    .send()
                    .await
                    .map_err(AwsError::from_sdk)?;
                let fields = json!({
                    "RouteId": r.route_id(),
                    "RouteKey": r.route_key(),
                    "Target": r.target(),
                    "AuthorizationType": r.authorization_type().map(|a| a.as_str()),
                    "AuthorizerId": r.authorizer_id(),
                    "ApiKeyRequired": r.api_key_required(),
                    "OperationName": r.operation_name(),
                });
                Ok(Detail {
                    fields,
                    raw: format!("{r:#?}"),
                })
            }
        }
    }
}

async fn list_rest(
    client: &RestClient,
    api: &str,
    position: Option<String>,
) -> Result<Page, AwsError> {
    let out = client
        .get_resources()
        .rest_api_id(api)
        .embed("methods") // otherwise each method is its own call
        .limit(500)
        .set_position(position)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let mut rows: Vec<(String, Row)> = Vec::new();
    for resource in out.items() {
        let path = resource.path().unwrap_or("/");
        for (method, m) in resource.resource_methods().into_iter().flatten() {
            let integration = m
                .method_integration()
                .map(|i| match i.uri() {
                    Some(uri) => integration_target(uri),
                    None => or_dash(i.r#type().map(|t| t.as_str())),
                })
                .unwrap_or_else(|| "-".into());
            let id = format!("{} {method}", resource.id().unwrap_or_default());
            let row = Row::new(
                id,
                vec![
                    method.as_str().into(),
                    path.into(),
                    integration.into(),
                    or_dash(m.authorization_type()).into(),
                ],
            );
            rows.push((format!("{path} {method}"), row));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Page {
        rows: rows.into_iter().map(|(_, r)| to_function(r)).collect(),
        next: out.position,
    })
}

/// v2 routes point at `integrations/<id>`; one GetIntegrations call per page
/// resolves those to what they call.
async fn list_v2(client: &V2Client, api: &str, token: Option<String>) -> Result<Page, AwsError> {
    let (routes, integrations) = tokio::join!(
        client
            .get_routes()
            .api_id(api)
            .max_results("100")
            .set_next_token(token)
            .send(),
        client
            .get_integrations()
            .api_id(api)
            .max_results("100")
            .send(),
    );
    let routes = routes.map_err(AwsError::from_sdk)?;
    let targets: HashMap<String, String> = integrations
        .map(|o| {
            o.items()
                .iter()
                .filter_map(|i| {
                    let id = i.integration_id()?.to_owned();
                    let target = match i.integration_uri() {
                        Some(uri) => integration_target(uri),
                        None => or_dash(i.integration_type().map(|t| t.as_str())),
                    };
                    Some((id, target))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut rows: Vec<Row> = routes
        .items()
        .iter()
        .map(|r| {
            let key = r.route_key().unwrap_or_default();
            // `GET /orders/{id}`, or a WebSocket key such as `$connect`.
            let (method, path) = key.split_once(' ').unwrap_or(("", key));
            let integration = r
                .target()
                .and_then(|t| t.strip_prefix("integrations/"))
                .map(|id| targets.get(id).cloned().unwrap_or_else(|| id.to_owned()))
                .unwrap_or_else(|| "-".into());
            Row::new(
                r.route_id().unwrap_or_default(),
                vec![
                    method.into(),
                    path.into(),
                    integration.into(),
                    or_dash(r.authorization_type().map(|a| a.as_str())).into(),
                ],
            )
        })
        .collect();
    rows.sort_by(|a, b| {
        (&a.cells[1].text, &a.cells[0].text).cmp(&(&b.cells[1].text, &b.cells[0].text))
    });
    Ok(Page {
        rows: rows.into_iter().map(to_function).collect(),
        next: routes.next_token,
    })
}

/// What `g` offers from a route: the Lambda function it integrates with.
fn to_function(row: Row) -> Row {
    let function = row.cells[2].text.strip_prefix("λ ").map(str::to_owned);
    match function {
        Some(f) => row.relate(format!("function {f}"), "lambda:function", &f),
        None => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_apigateway::operation::get_resources::GetResourcesOutput;
    use aws_sdk_apigateway::types::{Integration, Method, Resource};
    use aws_sdk_apigatewayv2::operation::get_integrations::GetIntegrationsOutput;
    use aws_sdk_apigatewayv2::operation::get_routes::GetRoutesOutput;
    use aws_sdk_apigatewayv2::types::{Integration as V2Integration, Route};
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    #[tokio::test]
    async fn rest_routes_expand_methods_and_name_lambda_targets() {
        let rule = mock!(RestClient::get_resources)
            .match_requests(|r| r.embed() == ["methods".to_owned()])
            .then_output(|| {
                let get = Method::builder()
                    .http_method("GET")
                    .authorization_type("NONE")
                    .method_integration(
                        Integration::builder()
                            .uri("arn:aws:apigateway:eu-west-1:lambda:path/2015-03-31/functions/arn:aws:lambda:eu-west-1:1:function:orders/invocations")
                            .build(),
                    )
                    .build();
                GetResourcesOutput::builder()
                    .items(
                        Resource::builder()
                            .id("r1")
                            .path("/orders")
                            .resource_methods("GET", get)
                            .build(),
                    )
                    .build()
            });
        let client = mock_client!(aws_sdk_apigateway, [&rule]);
        let page = list_rest(&client, "abc", None).await.unwrap();
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(cells, ["GET", "/orders", "λ orders", "NONE"]);
        assert_eq!(page.rows[0].id, "r1 GET");
        assert_eq!(page.rows[0].relations[0].filter.as_deref(), Some("orders"));
        assert_eq!(page.rows[0].relations[0].filter.as_deref(), Some("orders"));
    }

    #[tokio::test]
    async fn v2_routes_resolve_their_integrations() {
        let routes = mock!(V2Client::get_routes).then_output(|| {
            GetRoutesOutput::builder()
                .items(
                    Route::builder()
                        .route_id("rt1")
                        .route_key("POST /orders")
                        .target("integrations/int1")
                        .build(),
                )
                .items(
                    Route::builder()
                        .route_id("rt0")
                        .route_key("$connect")
                        .build(),
                )
                .build()
        });
        let integrations = mock!(V2Client::get_integrations).then_output(|| {
            GetIntegrationsOutput::builder()
                .items(
                    V2Integration::builder()
                        .integration_id("int1")
                        .integration_uri("arn:aws:lambda:eu-west-1:1:function:create-order")
                        .build(),
                )
                .build()
        });
        let client = mock_client!(
            aws_sdk_apigatewayv2,
            RuleMode::MatchAny,
            [&routes, &integrations]
        );
        let page = list_v2(&client, "xyz", None).await.unwrap();
        let first: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(first[..3], ["", "$connect", "-"]);
        let second: Vec<_> = page.rows[1].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(second[..3], ["POST", "/orders", "λ create-order"]);
    }
}
