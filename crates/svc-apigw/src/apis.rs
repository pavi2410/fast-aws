//! `apigw:api`: REST, HTTP and WebSocket APIs in one list.

use async_trait::async_trait;
use aws_sdk_apigateway::Client as RestClient;
use aws_sdk_apigatewayv2::Client as V2Client;
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

use crate::{Flavor, split_id, tagged_id};

pub struct Apis;

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("TYPE", 9),
    Column::fixed("ID", 10),
    Column::fill("ENDPOINT", 3),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Apis {
    kind_info! {
        id: "apigw:api",
        title: "API Gateway APIs",
        aliases: ["apigw", "api", "apis"],
        scope: Scope::Regional,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let rest = RestClient::new(ctx.config());
        let v2 = V2Client::new(ctx.config());
        list(&rest, &v2, &ctx.region, query.next.as_deref()).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        match split_id(id) {
            (Flavor::Rest, id) => describe_rest(&RestClient::new(ctx.config()), id).await,
            (Flavor::V2, id) => describe_v2(&V2Client::new(ctx.config()), id).await,
        }
    }
}

/// Lists REST APIs first, then v2 ones. The page token says which API to call
/// next: `rest:<position>` or `v2:<token>` (`v2:` to start v2).
///
/// If one of the two is denied, its error becomes a row and the other one's
/// APIs still show; only both failing fails the list.
async fn list(
    rest: &RestClient,
    v2: &V2Client,
    region: &str,
    next: Option<&str>,
) -> Result<Page, AwsError> {
    match next {
        None | Some("rest:") => list_rest(rest, region, None).await,
        Some(t) if t.starts_with("rest:") => list_rest(rest, region, Some(&t[5..])).await,
        Some(t) => list_v2(v2, t.strip_prefix("v2:").filter(|t| !t.is_empty())).await,
    }
    .or_else(|err| {
        // REST failed: show why, and carry on with v2 on the next page.
        if next.is_none_or(|t| t.starts_with("rest:")) {
            Ok(Page {
                rows: vec![error_row("REST APIs", &err)],
                next: Some("v2:".into()),
            })
        } else {
            Err(err)
        }
    })
}

fn error_row(what: &str, err: &AwsError) -> Row {
    Row::new(
        format!("⚠ {what}"),
        vec![
            Cell::toned(format!("⚠ {what}: {err}"), Tone::Bad),
            "".into(),
            "".into(),
            "".into(),
            "".into(),
        ],
    )
}

async fn list_rest(
    client: &RestClient,
    region: &str,
    position: Option<&str>,
) -> Result<Page, AwsError> {
    let out = client
        .get_rest_apis()
        .limit(500)
        .set_position(position.map(str::to_owned))
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .items()
        .iter()
        .map(|api| {
            let id = api.id().unwrap_or_default();
            let endpoint = format!("https://{id}.execute-api.{region}.amazonaws.com");
            api_row(
                Flavor::Rest,
                id,
                api.name(),
                "REST",
                &endpoint,
                api.created_date().map(|d| d.secs()),
            )
        })
        .collect();
    let next = match out.position() {
        Some(p) => format!("rest:{p}"),
        None => "v2:".to_owned(),
    };
    Ok(Page {
        rows,
        next: Some(next),
    })
}

async fn list_v2(client: &V2Client, token: Option<&str>) -> Result<Page, AwsError> {
    let out = client
        .get_apis()
        .max_results("100")
        .set_next_token(token.map(str::to_owned))
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .items()
        .iter()
        .map(|api| {
            let kind = api.protocol_type().map(|p| p.as_str()).unwrap_or("HTTP");
            api_row(
                Flavor::V2,
                api.api_id().unwrap_or_default(),
                api.name(),
                kind,
                api.api_endpoint().unwrap_or_default(),
                api.created_date().map(|d| d.secs()),
            )
        })
        .collect();
    Ok(Page {
        rows,
        next: out.next_token().map(|t| format!("v2:{t}")),
    })
}

fn api_row(
    flavor: Flavor,
    id: &str,
    name: Option<&str>,
    kind: &str,
    endpoint: &str,
    created: Option<i64>,
) -> Row {
    let tagged = tagged_id(flavor, id);
    let parent = Parent {
        id: tagged.clone(),
        label: or_dash(name),
        region: None,
    };
    Row::new(
        tagged,
        vec![
            or_dash(name).into(),
            kind.into(),
            id.into(),
            endpoint.into(),
            age_of(created).into(),
        ],
    )
    .drill("apigw:route", parent)
}

/// A REST API with its stages.
async fn describe_rest(client: &RestClient, id: &str) -> Result<Detail, AwsError> {
    let (api, stages) = tokio::join!(
        client.get_rest_api().rest_api_id(id).send(),
        client.get_stages().rest_api_id(id).send(),
    );
    let api = api.map_err(AwsError::from_sdk)?;
    let stages: Value = match stages {
        Ok(s) => json!(s.item().iter().map(|s| json!({
            "Name": s.stage_name(),
            "DeploymentId": s.deployment_id(),
            "TracingEnabled": s.tracing_enabled(),
            "CacheEnabled": s.cache_cluster_enabled(),
            "AccessLogDestination": s.access_log_settings().and_then(|l| l.destination_arn()),
            "WebAclArn": s.web_acl_arn(),
            "Variables": s.variables(),
            "LastUpdated": s.last_updated_date().map(|d| d.to_string()),
        })).collect::<Vec<_>>()),
        Err(e) => json!(format!("⚠ {}", AwsError::from_sdk(e))),
    };
    let fields = json!({
        "Id": api.id(),
        "Name": api.name(),
        "Type": "REST",
        "Description": api.description(),
        "EndpointTypes": api.endpoint_configuration().map(|e| e.types().iter().map(|t| t.as_str()).collect::<Vec<_>>()),
        "ApiKeySource": api.api_key_source().map(|s| s.as_str()),
        "DisableExecuteApiEndpoint": api.disable_execute_api_endpoint(),
        "CreatedDate": api.created_date().map(|d| d.to_string()),
        "Stages": stages,
        "Tags": api.tags(),
    });
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

/// An HTTP or WebSocket API with its stages.
async fn describe_v2(client: &V2Client, id: &str) -> Result<Detail, AwsError> {
    let (api, stages) = tokio::join!(
        client.get_api().api_id(id).send(),
        client.get_stages().api_id(id).send(),
    );
    let api = api.map_err(AwsError::from_sdk)?;
    let stages: Value = match stages {
        Ok(s) => json!(s.items().iter().map(|s| json!({
            "Name": s.stage_name(),
            "AutoDeploy": s.auto_deploy(),
            "DeploymentId": s.deployment_id(),
            "AccessLogDestination": s.access_log_settings().and_then(|l| l.destination_arn()),
            "Variables": s.stage_variables(),
            "LastUpdated": s.last_updated_date().map(|d| d.to_string()),
        })).collect::<Vec<_>>()),
        Err(e) => json!(format!("⚠ {}", AwsError::from_sdk(e))),
    };
    let fields = json!({
        "ApiId": api.api_id(),
        "Name": api.name(),
        "Type": api.protocol_type().map(|p| p.as_str()),
        "Endpoint": api.api_endpoint(),
        "Description": api.description(),
        "RouteSelectionExpression": api.route_selection_expression(),
        "Cors": api.cors_configuration().map(|c| json!({
            "AllowOrigins": c.allow_origins(),
            "AllowMethods": c.allow_methods(),
        })),
        "CreatedDate": api.created_date().map(|d| d.to_string()),
        "Stages": stages,
        "Tags": api.tags(),
    });
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_apigateway::error::ErrorMetadata;
    use aws_sdk_apigateway::operation::get_rest_apis::{GetRestApisError, GetRestApisOutput};
    use aws_sdk_apigateway::types::RestApi;
    use aws_sdk_apigatewayv2::operation::get_apis::GetApisOutput;
    use aws_sdk_apigatewayv2::types::{Api, ProtocolType};
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn rest_apis_come_first_then_v2() {
        let rest_rule = mock!(RestClient::get_rest_apis).then_output(|| {
            GetRestApisOutput::builder()
                .items(RestApi::builder().id("abc").name("orders").build())
                .build()
        });
        let v2_rule = mock!(V2Client::get_apis).then_output(|| {
            GetApisOutput::builder()
                .items(
                    Api::builder()
                        .api_id("xyz")
                        .name("chat")
                        .protocol_type(ProtocolType::Websocket)
                        .api_endpoint("wss://xyz.execute-api.eu-west-1.amazonaws.com")
                        .route_selection_expression("$request.body.action")
                        .build(),
                )
                .next_token("n2")
                .build()
        });
        let rest = mock_client!(aws_sdk_apigateway, [&rest_rule]);
        let v2 = mock_client!(aws_sdk_apigatewayv2, [&v2_rule]);

        let first = list(&rest, &v2, "eu-west-1", None).await.unwrap();
        assert_eq!(first.rows[0].id, "rest:abc");
        assert_eq!(
            first.rows[0].cells[3].text,
            "https://abc.execute-api.eu-west-1.amazonaws.com"
        );
        assert_eq!(first.next.as_deref(), Some("v2:"));

        let second = list(&rest, &v2, "eu-west-1", first.next.as_deref())
            .await
            .unwrap();
        assert_eq!(second.rows[0].id, "v2:xyz");
        assert_eq!(second.rows[0].cells[1].text, "WEBSOCKET");
        assert_eq!(second.next.as_deref(), Some("v2:n2"));
        assert_eq!(second.rows[0].drill.as_ref().unwrap().parent.id, "v2:xyz");
    }

    #[tokio::test]
    async fn a_denied_rest_api_list_still_moves_on_to_v2() {
        let rest_rule = mock!(RestClient::get_rest_apis).then_error(|| {
            GetRestApisError::generic(
                ErrorMetadata::builder()
                    .code("AccessDeniedException")
                    .build(),
            )
        });
        let rest = mock_client!(aws_sdk_apigateway, [&rest_rule]);
        let v2 = mock_client!(aws_sdk_apigatewayv2, []);

        let page = list(&rest, &v2, "eu-west-1", None).await.unwrap();
        assert!(page.rows[0].cells[0].text.contains("AccessDeniedException"));
        assert_eq!(page.rows[0].cells[0].tone, Tone::Bad);
        assert_eq!(page.next.as_deref(), Some("v2:"));
    }
}
