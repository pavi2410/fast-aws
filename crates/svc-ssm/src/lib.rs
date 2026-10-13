//! SSM Parameter Store: parameters.
//!
//! Values are read with `WithDecryption=false`, and SecureString values are never
//! shown: revealing them (a deliberate key press, recorded in CloudTrail) comes
//! with the planned secret-field widget.

use async_trait::async_trait;
use aws_sdk_ssm::Client;
use aws_sdk_ssm::types::{ParameterMetadata, ParameterStringFilter, ParameterType};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

/// Registers every SSM resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Parameters);
}

const MASK: &str = "•••••••• (SecureString, not decrypted)";

pub struct Parameters;

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 4),
    Column::fixed("TYPE", 12),
    Column::fixed("VERSION", 7),
    Column::fixed("TIER", 12),
    Column::fixed("MODIFIED", 8),
    Column::fill("BY", 2),
];

#[async_trait]
impl ResourceKind for Parameters {
    kind_info! {
        id: "ssm:param",
        title: "SSM Parameters",
        aliases: ["ssm", "param", "params", "parameters"],
        scope: Scope::Regional,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        _: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        describe(&Client::new(ctx.config()), name).await
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .describe_parameters()
        .max_results(50)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.parameters().iter().map(row).collect(),
        next: out.next_token,
    })
}

fn row(p: &ParameterMetadata) -> Row {
    let kind = p.r#type().map(|t| t.as_str()).unwrap_or("-");
    let kind = if kind == "SecureString" {
        Cell::toned(kind, Tone::Warn)
    } else {
        Cell::new(kind)
    };
    // Last-modified-by is an ARN; the user or role name is the useful part.
    let by = p
        .last_modified_user()
        .map(|u| u.rsplit('/').next().unwrap_or(u));
    Row::new(
        p.name().unwrap_or_default(),
        vec![
            or_dash(p.name()).into(),
            kind,
            p.version().to_string().into(),
            or_dash(p.tier().map(|t| t.as_str())).into(),
            age_of(p.last_modified_date().map(|d| d.secs())).into(),
            or_dash(by).into(),
        ],
    )
}

async fn describe(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let name_filter = ParameterStringFilter::builder()
        .key("Name")
        .option("Equals")
        .values(name)
        .build()
        .map_err(|e| AwsError::new(fast_aws_core::ErrorKind::Other, None, e.to_string()))?;
    let (value, meta) = tokio::join!(
        client
            .get_parameter()
            .name(name)
            .with_decryption(false)
            .send(),
        client
            .describe_parameters()
            .parameter_filters(name_filter)
            .send(),
    );
    let value = value.map_err(AwsError::from_sdk)?;
    let p = value.parameter().ok_or_else(|| AwsError::not_found(name))?;
    let shown = match p.r#type() {
        Some(ParameterType::SecureString) => MASK.to_owned(),
        _ => p.value().unwrap_or_default().to_owned(),
    };
    let meta = meta.ok();
    let m = meta.as_ref().and_then(|m| m.parameters().first());
    let fields = json!({
        "Name": p.name(),
        "Arn": p.arn(),
        "Type": p.r#type().map(|t| t.as_str()),
        "Value": shown,
        "Version": p.version(),
        "DataType": p.data_type(),
        "Tier": m.and_then(|m| m.tier()).map(|t| t.as_str()),
        "Description": m.and_then(|m| m.description()),
        "KmsKeyId": m.and_then(|m| m.key_id()),
        "AllowedPattern": m.and_then(|m| m.allowed_pattern()),
        "LastModifiedDate": p.last_modified_date().map(|d| d.to_string()),
        "LastModifiedUser": m.and_then(|m| m.last_modified_user()),
    });
    // The raw SDK output would include the (encrypted) value, so raw shows the
    // same masked fields.
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_ssm::operation::describe_parameters::DescribeParametersOutput;
    use aws_sdk_ssm::operation::get_parameter::GetParameterOutput;
    use aws_sdk_ssm::types::Parameter;
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    #[tokio::test]
    async fn parameters_page_and_flag_secure_strings() {
        let rule = mock!(Client::describe_parameters)
            .match_requests(|r| r.next_token() == Some("t1"))
            .then_output(|| {
                DescribeParametersOutput::builder()
                    .parameters(
                        ParameterMetadata::builder()
                            .name("/prod/db/password")
                            .r#type(ParameterType::SecureString)
                            .version(3)
                            .last_modified_user("arn:aws:sts::1:assumed-role/Admin/alice")
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_ssm, [&rule]);
        let page = list(
            &client,
            &ListQuery {
                parent: None,
                next: Some("t1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.next.as_deref(), Some("t2"));
        let row = &page.rows[0];
        assert_eq!(row.cells[1].tone, Tone::Warn);
        assert_eq!(row.cells[2].text, "3");
        assert_eq!(row.cells[5].text, "alice");
    }

    #[tokio::test]
    async fn secure_string_values_are_never_shown_or_decrypted() {
        let get = mock!(Client::get_parameter)
            .match_requests(|r| r.with_decryption() == Some(false))
            .then_output(|| {
                GetParameterOutput::builder()
                    .parameter(
                        Parameter::builder()
                            .name("/prod/db/password")
                            .r#type(ParameterType::SecureString)
                            .value("AQICAHh-ciphertext")
                            .build(),
                    )
                    .build()
            });
        let meta = mock!(Client::describe_parameters)
            .then_output(|| DescribeParametersOutput::builder().build());
        let client = mock_client!(aws_sdk_ssm, RuleMode::MatchAny, [&get, &meta]);

        let d = describe(&client, "/prod/db/password").await.unwrap();
        assert_eq!(d.fields["Value"], MASK);
        assert!(!d.raw.contains("ciphertext"));
        assert_eq!(get.num_calls(), 1);
    }

    #[tokio::test]
    async fn plain_strings_show_their_value() {
        let get = mock!(Client::get_parameter).then_output(|| {
            GetParameterOutput::builder()
                .parameter(
                    Parameter::builder()
                        .name("/prod/feature_flags")
                        .r#type(ParameterType::String)
                        .value("beta=on")
                        .build(),
                )
                .build()
        });
        let meta = mock!(Client::describe_parameters)
            .then_output(|| DescribeParametersOutput::builder().build());
        let client = mock_client!(aws_sdk_ssm, RuleMode::MatchAny, [&get, &meta]);
        let d = describe(&client, "/prod/feature_flags").await.unwrap();
        assert_eq!(d.fields["Value"], "beta=on");
    }
}
