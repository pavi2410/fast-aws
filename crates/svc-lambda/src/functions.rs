//! `lambda:function`: Lambda functions.

use async_trait::async_trait;
use aws_sdk_lambda::Client;
use aws_sdk_lambda::types::FunctionConfiguration;
use fast_aws_core::fmt::{bytes, date_part, or_dash};
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, kind_info,
};
use serde_json::{Map, Value, json};

pub struct Functions;

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("RUNTIME", 13),
    Column::fixed("MEMORY", 7),
    Column::fixed("TIMEOUT", 7),
    Column::fixed("CODE", 9),
    Column::fixed("MODIFIED", 10),
];

#[async_trait]
impl ResourceKind for Functions {
    kind_info! {
        id: "lambda:function",
        title: "Lambda Functions",
        aliases: ["lambda", "fn", "functions"],
        scope: Scope::Regional,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        describe(&Client::new(ctx.config()), id).await
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .list_functions()
        .max_items(50)
        .set_marker(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.functions().iter().map(row).collect(),
        next: out.next_marker,
    })
}

fn row(f: &FunctionConfiguration) -> Row {
    // Container-image functions have a package type instead of a runtime.
    let runtime = match f.runtime() {
        Some(r) => r.as_str().to_owned(),
        None => or_dash(f.package_type().map(|p| p.as_str())),
    };
    Row::new(
        f.function_name().unwrap_or_default(),
        vec![
            or_dash(f.function_name()).into(),
            runtime.into(),
            f.memory_size()
                .map(|m| format!("{m} MB"))
                .unwrap_or_default()
                .into(),
            f.timeout()
                .map(|t| format!("{t}s"))
                .unwrap_or_default()
                .into(),
            bytes(f.code_size()).into(),
            date_part(f.last_modified()).into(),
        ],
    )
}

/// Environment variables whose names suggest a secret are masked. Revealing
/// values belongs to the planned config panel, behind a key press.
fn looks_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "secret",
        "password",
        "passwd",
        "token",
        "key",
        "credential",
        "private",
        "auth",
    ]
    .iter()
    .any(|w| name.contains(w))
}

fn environment(f: &FunctionConfiguration) -> Value {
    let vars = f.environment().and_then(|e| e.variables());
    let map: Map<String, Value> = vars
        .into_iter()
        .flatten()
        .map(|(k, v)| {
            let shown = if looks_secret(k) {
                "••••••••".to_owned()
            } else {
                v.clone()
            };
            (k.clone(), json!(shown))
        })
        .collect();
    Value::Object(map)
}

async fn describe(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let out = client
        .get_function()
        .function_name(name)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let f = out
        .configuration()
        .ok_or_else(|| AwsError::not_found(name))?;
    let vpc = f
        .vpc_config()
        .filter(|v| v.vpc_id().is_some_and(|id| !id.is_empty()));
    let fields = json!({
        "FunctionName": f.function_name(),
        "FunctionArn": f.function_arn(),
        "Description": f.description(),
        "Runtime": f.runtime().map(|r| r.as_str()),
        "PackageType": f.package_type().map(|p| p.as_str()),
        "Handler": f.handler(),
        "Architectures": f.architectures().iter().map(|a| a.as_str()).collect::<Vec<_>>(),
        "MemorySize": f.memory_size(),
        "Timeout": f.timeout(),
        "EphemeralStorage": f.ephemeral_storage().map(|e| e.size()),
        "Role": f.role(),
        "State": f.state().map(|s| s.as_str()),
        "LastUpdateStatus": f.last_update_status().map(|s| s.as_str()),
        "LastModified": f.last_modified(),
        "CodeSize": bytes(f.code_size()),
        "Version": f.version(),
        "LogGroup": f.logging_config().and_then(|l| l.log_group()),
        "Layers": f.layers().iter().filter_map(|l| l.arn()).collect::<Vec<_>>(),
        "Vpc": vpc.map(|v| json!({
            "VpcId": v.vpc_id(),
            "SubnetIds": v.subnet_ids(),
            "SecurityGroupIds": v.security_group_ids(),
        })),
        "Environment": environment(f),
        "ReservedConcurrency": out.concurrency().and_then(|c| c.reserved_concurrent_executions()),
        "Tags": out.tags(),
    });
    // The raw view would show the environment unmasked, so it shows the same
    // curated fields instead.
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_lambda::operation::get_function::GetFunctionOutput;
    use aws_sdk_lambda::operation::list_functions::ListFunctionsOutput;
    use aws_sdk_lambda::types::{EnvironmentResponse, Runtime};
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn functions_page_with_markers() {
        let rule = mock!(Client::list_functions)
            .match_requests(|r| r.marker() == Some("m1"))
            .then_output(|| {
                ListFunctionsOutput::builder()
                    .functions(
                        FunctionConfiguration::builder()
                            .function_name("thumbs")
                            .runtime(Runtime::Python313)
                            .memory_size(512)
                            .timeout(30)
                            .code_size(2048)
                            .last_modified("2026-10-12T09:13:44.000+0000")
                            .build(),
                    )
                    .next_marker("m2")
                    .build()
            });
        let client = mock_client!(aws_sdk_lambda, [&rule]);
        let page = list(
            &client,
            &ListQuery {
                parent: None,
                next: Some("m1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.next.as_deref(), Some("m2"));
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            cells,
            [
                "thumbs",
                "python3.13",
                "512 MB",
                "30s",
                "2.0 KiB",
                "2026-10-12"
            ]
        );
    }

    #[tokio::test]
    async fn describe_masks_secret_looking_environment_values() {
        let rule = mock!(Client::get_function).then_output(|| {
            GetFunctionOutput::builder()
                .configuration(
                    FunctionConfiguration::builder()
                        .function_name("thumbs")
                        .environment(
                            EnvironmentResponse::builder()
                                .variables("BUCKET", "images")
                                .variables("STRIPE_API_KEY", "sk_live_123")
                                .variables("DB_PASSWORD", "hunter2")
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        let client = mock_client!(aws_sdk_lambda, [&rule]);
        let d = describe(&client, "thumbs").await.unwrap();
        assert_eq!(d.fields["Environment"]["BUCKET"], "images");
        assert_eq!(d.fields["Environment"]["STRIPE_API_KEY"], "••••••••");
        assert_eq!(d.fields["Environment"]["DB_PASSWORD"], "••••••••");
        assert!(!d.raw.contains("sk_live_123") && !d.raw.contains("hunter2"));
    }
}
