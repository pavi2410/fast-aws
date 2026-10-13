//! Secrets Manager: secrets, metadata only.
//!
//! Listing and describing never read values. Only [`reveal`] calls
//! `GetSecretValue`, after `v` and a typed confirmation: it's billed, audited
//! in CloudTrail, and puts the value on screen.

mod reveal;

use async_trait::async_trait;
use aws_sdk_secretsmanager::Client;
use aws_sdk_secretsmanager::types::{SecretListEntry, Tag};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

/// Registers every Secrets Manager resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Secrets);
    registry.register(reveal::Values);
}

pub struct Secrets;

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("ROTATION", 8),
    Column::fixed("CHANGED", 7),
    Column::fixed("ACCESSED", 8),
    Column::fixed("OWNER", 8),
    Column::fill("DESCRIPTION", 3),
];

#[async_trait]
impl ResourceKind for Secrets {
    kind_info! {
        id: "secrets:secret",
        title: "Secrets Manager",
        aliases: ["secrets", "secret", "sm"],
        scope: Scope::Regional,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let s = Client::new(ctx.config())
            .describe_secret()
            .secret_id(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let fields = json!({
            "Name": s.name(),
            "Arn": s.arn(),
            "Description": s.description(),
            "KmsKeyId": s.kms_key_id(),
            "RotationEnabled": s.rotation_enabled(),
            "RotationLambdaArn": s.rotation_lambda_arn(),
            "RotationSchedule": s.rotation_rules().map(|r| json!({
                "AutomaticallyAfterDays": r.automatically_after_days(),
                "ScheduleExpression": r.schedule_expression(),
            })),
            "LastRotatedDate": s.last_rotated_date().map(|d| d.to_string()),
            "NextRotationDate": s.next_rotation_date().map(|d| d.to_string()),
            "LastChangedDate": s.last_changed_date().map(|d| d.to_string()),
            "LastAccessedDate": s.last_accessed_date().map(|d| d.to_string()),
            "DeletedDate": s.deleted_date().map(|d| d.to_string()),
            "OwningService": s.owning_service(),
            "PrimaryRegion": s.primary_region(),
            "Replicas": s.replication_status().iter().map(|r| json!({
                "Region": r.region(),
                "Status": r.status().map(|s| s.as_str()),
            })).collect::<Vec<_>>(),
            "CreatedDate": s.created_date().map(|d| d.to_string()),
            "Tags": tags(s.tags()),
        });
        Ok(Detail {
            fields,
            raw: format!("{s:#?}"),
        })
    }
}

fn tags(tags: &[Tag]) -> Value {
    Value::Object(
        tags.iter()
            .filter_map(|t| Some((t.key()?.to_owned(), json!(t.value()))))
            .collect(),
    )
}

/// Includes secrets scheduled for deletion, marked, so they aren't a surprise.
async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .list_secrets()
        .include_planned_deletion(true)
        .max_results(100)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out
            .secret_list()
            .iter()
            .map(|s| {
                let id = s.arn().or(s.name()).unwrap_or_default();
                row(s).query('v', reveal::PROMPT, "secrets:value", id)
            })
            .collect(),
        next: out.next_token,
    })
}

fn row(s: &SecretListEntry) -> Row {
    let name = s.name().unwrap_or_default();
    let name_cell = if s.deleted_date().is_some() {
        Cell::toned(format!("{name} (deleting)"), Tone::Bad)
    } else {
        Cell::new(name)
    };
    let rotation = match s.rotation_enabled() {
        Some(true) => Cell::toned("on", Tone::Good),
        _ => Cell::toned("off", Tone::Dim),
    };
    let accessed = match s.last_accessed_date() {
        Some(d) => age_of(Some(d.secs())),
        None => "never".into(),
    };
    Row::new(
        s.arn().unwrap_or(name),
        vec![
            name_cell,
            rotation,
            age_of(s.last_changed_date().map(|d| d.secs())).into(),
            accessed.into(),
            or_dash(s.owning_service()).into(),
            or_dash(s.description()).into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::primitives::DateTime;
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn secrets_list_metadata_including_planned_deletions() {
        let rule = mock!(Client::list_secrets)
            .match_requests(|r| {
                r.include_planned_deletion() == Some(true) && r.next_token() == Some("t1")
            })
            .then_output(|| {
                ListSecretsOutput::builder()
                    .secret_list(
                        SecretListEntry::builder()
                            .name("prod/stripe")
                            .arn("arn:aws:secretsmanager:eu-west-1:1:secret:prod/stripe-AbC")
                            .rotation_enabled(true)
                            .build(),
                    )
                    .secret_list(
                        SecretListEntry::builder()
                            .name("old")
                            .arn("arn:old")
                            .deleted_date(DateTime::from_secs(0))
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_secretsmanager, [&rule]);
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
        assert_eq!(
            page.rows[0].id,
            "arn:aws:secretsmanager:eu-west-1:1:secret:prod/stripe-AbC"
        );
        assert_eq!(page.rows[0].cells[1].tone, Tone::Good);
        assert_eq!(page.rows[0].cells[3].text, "never");
        assert_eq!(page.rows[1].cells[0].text, "old (deleting)");
        assert_eq!(page.rows[1].cells[0].tone, Tone::Bad);
    }
}
