//! DynamoDB resource kinds: tables.
//!
//! Tables, and their items by partition key (`Q`, see [`items`]). There is no
//! Scan: it reads the whole table and consumes its read capacity.

mod items;

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{KeySchemaElement, KeyType, TableDescription};
use fast_aws_core::fmt::bytes;
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, kind_info,
};
use serde_json::{Value, json};

/// Registers every DynamoDB resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Tables);
    registry.register(items::Items);
}

pub struct Tables;

/// `ListTables` returns names only; everything else would be one
/// `DescribeTable` per row, so it's left to describe.
const COLUMNS: &[Column] = &[Column::fill("NAME", 1)];

#[async_trait]
impl ResourceKind for Tables {
    kind_info! {
        id: "dynamodb:table",
        title: "DynamoDB Tables",
        aliases: ["ddb", "dynamo", "dynamodb", "tables"],
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
        .list_tables()
        .limit(100)
        .set_exclusive_start_table_name(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .table_names()
        .iter()
        .map(|n| {
            Row::new(n, vec![n.as_str().into()]).query(
                'Q',
                "query items",
                items::PROMPT,
                "dynamodb:item",
                n,
            )
        })
        .collect();
    Ok(Page {
        rows,
        next: out.last_evaluated_table_name,
    })
}

/// `pk (S)`, `sk (N)`: key attribute names with their types.
fn keys(schema: &[KeySchemaElement], t: &TableDescription) -> Value {
    let type_of = |name: &str| {
        t.attribute_definitions()
            .iter()
            .find(|a| a.attribute_name() == name)
            .map(|a| a.attribute_type().as_str().to_owned())
            .unwrap_or_default()
    };
    let key = |kind: KeyType| {
        schema
            .iter()
            .find(|k| *k.key_type() == kind)
            .map(|k| format!("{} ({})", k.attribute_name(), type_of(k.attribute_name())))
    };
    json!({ "Partition": key(KeyType::Hash), "Sort": key(KeyType::Range) })
}

async fn describe(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let (table, backups, ttl) = tokio::join!(
        client.describe_table().table_name(name).send(),
        client.describe_continuous_backups().table_name(name).send(),
        client.describe_time_to_live().table_name(name).send(),
    );
    let table = table.map_err(AwsError::from_sdk)?;
    let t = table.table().ok_or_else(|| AwsError::not_found(name))?;
    let billing = t
        .billing_mode_summary()
        .and_then(|b| b.billing_mode())
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| "PROVISIONED".into());
    let pitr = backups.map_err(AwsError::from_sdk).map(|b| {
        json!(
            b.continuous_backups_description()
                .and_then(|d| d.point_in_time_recovery_description())
                .and_then(|p| p.point_in_time_recovery_status())
                .map(|s| s.as_str())
        )
    });
    let ttl = ttl.map_err(AwsError::from_sdk).map(|t| {
        let d = t.time_to_live_description();
        json!({
            "Status": d.and_then(|d| d.time_to_live_status()).map(|s| s.as_str()),
            "Attribute": d.and_then(|d| d.attribute_name()),
        })
    });
    let or_error = |r: Result<Value, AwsError>| r.unwrap_or_else(|e| json!(format!("⚠ {e}")));
    let fields = json!({
        "TableName": t.table_name(),
        "Arn": t.table_arn(),
        "Status": t.table_status().map(|s| s.as_str()),
        "Keys": keys(t.key_schema(), t),
        "BillingMode": billing,
        "ProvisionedThroughput": t.provisioned_throughput().filter(|_| billing == "PROVISIONED").map(|p| json!({
            "Read": p.read_capacity_units(),
            "Write": p.write_capacity_units(),
        })),
        // Both are refreshed by DynamoDB only about every six hours.
        "ItemCountApprox": t.item_count(),
        "SizeApprox": t.table_size_bytes().map(bytes),
        "TableClass": t.table_class_summary().and_then(|c| c.table_class()).map(|c| c.as_str()),
        "GlobalSecondaryIndexes": t.global_secondary_indexes().iter().map(|g| json!({
            "Name": g.index_name(),
            "Keys": keys(g.key_schema(), t),
            "Status": g.index_status().map(|s| s.as_str()),
        })).collect::<Vec<_>>(),
        "LocalSecondaryIndexes": t.local_secondary_indexes().iter().map(|l| l.index_name()).collect::<Vec<_>>(),
        "Stream": t.stream_specification().filter(|s| s.stream_enabled()).map(|s| json!({
            "ViewType": s.stream_view_type().map(|v| v.as_str()),
            "Arn": t.latest_stream_arn(),
        })),
        "Replicas": t.replicas().iter().map(|r| r.region_name()).collect::<Vec<_>>(),
        "PointInTimeRecovery": or_error(pitr),
        "TimeToLive": or_error(ttl),
        "DeletionProtection": t.deletion_protection_enabled(),
        "Encryption": t.sse_description().and_then(|s| s.sse_type()).map(|s| s.as_str()),
        "CreationDateTime": t.creation_date_time().map(|d| d.to_string()),
    });
    Ok(Detail {
        fields,
        raw: format!("{t:#?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::operation::describe_continuous_backups::DescribeContinuousBackupsOutput;
    use aws_sdk_dynamodb::operation::describe_table::DescribeTableOutput;
    use aws_sdk_dynamodb::operation::describe_time_to_live::DescribeTimeToLiveOutput;
    use aws_sdk_dynamodb::operation::list_tables::ListTablesOutput;
    use aws_sdk_dynamodb::types::{
        AttributeDefinition, BillingMode, BillingModeSummary, ContinuousBackupsDescription,
        ContinuousBackupsStatus, PointInTimeRecoveryDescription, PointInTimeRecoveryStatus,
        ScalarAttributeType,
    };
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    #[tokio::test]
    async fn tables_page_by_last_evaluated_name() {
        let rule = mock!(Client::list_tables)
            .match_requests(|r| r.exclusive_start_table_name() == Some("a"))
            .then_output(|| {
                ListTablesOutput::builder()
                    .table_names("orders")
                    .last_evaluated_table_name("orders")
                    .build()
            });
        let client = mock_client!(aws_sdk_dynamodb, [&rule]);
        let page = list(
            &client,
            &ListQuery {
                parent: None,
                next: Some("a".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.rows[0].id, "orders");
        assert_eq!(page.next.as_deref(), Some("orders"));
    }

    #[tokio::test]
    async fn describe_shows_typed_keys_billing_and_pitr() {
        let table = mock!(Client::describe_table).then_output(|| {
            DescribeTableOutput::builder()
                .table(
                    TableDescription::builder()
                        .table_name("orders")
                        .key_schema(
                            KeySchemaElement::builder()
                                .attribute_name("pk")
                                .key_type(KeyType::Hash)
                                .build()
                                .unwrap(),
                        )
                        .key_schema(
                            KeySchemaElement::builder()
                                .attribute_name("sk")
                                .key_type(KeyType::Range)
                                .build()
                                .unwrap(),
                        )
                        .attribute_definitions(
                            AttributeDefinition::builder()
                                .attribute_name("pk")
                                .attribute_type(ScalarAttributeType::S)
                                .build()
                                .unwrap(),
                        )
                        .attribute_definitions(
                            AttributeDefinition::builder()
                                .attribute_name("sk")
                                .attribute_type(ScalarAttributeType::N)
                                .build()
                                .unwrap(),
                        )
                        .billing_mode_summary(
                            BillingModeSummary::builder()
                                .billing_mode(BillingMode::PayPerRequest)
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        let backups = mock!(Client::describe_continuous_backups).then_output(|| {
            DescribeContinuousBackupsOutput::builder()
                .continuous_backups_description(
                    ContinuousBackupsDescription::builder()
                        .continuous_backups_status(ContinuousBackupsStatus::Enabled)
                        .point_in_time_recovery_description(
                            PointInTimeRecoveryDescription::builder()
                                .point_in_time_recovery_status(PointInTimeRecoveryStatus::Enabled)
                                .build(),
                        )
                        .build()
                        .unwrap(),
                )
                .build()
        });
        let ttl = mock!(Client::describe_time_to_live)
            .then_output(|| DescribeTimeToLiveOutput::builder().build());
        let client = mock_client!(
            aws_sdk_dynamodb,
            RuleMode::MatchAny,
            [&table, &backups, &ttl]
        );

        let d = describe(&client, "orders").await.unwrap();
        assert_eq!(d.fields["Keys"]["Partition"], "pk (S)");
        assert_eq!(d.fields["Keys"]["Sort"], "sk (N)");
        assert_eq!(d.fields["BillingMode"], "PAY_PER_REQUEST");
        assert_eq!(d.fields["ProvisionedThroughput"], Value::Null);
        assert_eq!(d.fields["PointInTimeRecovery"], "ENABLED");
    }
}
