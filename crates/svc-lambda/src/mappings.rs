//! `lambda:esm`: event source mappings (SQS, DynamoDB/Kinesis streams, … → function).

use async_trait::async_trait;
use aws_sdk_lambda::Client;
use aws_sdk_lambda::types::EventSourceMappingConfiguration;
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

use crate::arn_name;

pub struct EventSourceMappings;

const COLUMNS: &[Column] = &[
    Column::fill("SOURCE", 3),
    Column::fill("FUNCTION", 2),
    Column::fixed("STATE", 10),
    Column::fixed("BATCH", 5),
    Column::fixed("MODIFIED", 8),
    Column::fixed("UUID", 36),
];

#[async_trait]
impl ResourceKind for EventSourceMappings {
    kind_info! {
        id: "lambda:esm",
        title: "Event Source Mappings",
        aliases: ["esm", "triggers"],
        scope: Scope::Regional,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let m = Client::new(ctx.config())
            .get_event_source_mapping()
            .uuid(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let fields = json!({
            "UUID": m.uuid(),
            "EventSourceArn": m.event_source_arn(),
            "FunctionArn": m.function_arn(),
            "State": m.state(),
            "StateTransitionReason": m.state_transition_reason(),
            "BatchSize": m.batch_size(),
            "MaximumBatchingWindowInSeconds": m.maximum_batching_window_in_seconds(),
            "StartingPosition": m.starting_position().map(|p| p.as_str()),
            "LastProcessingResult": m.last_processing_result(),
            "LastModified": m.last_modified().map(|t| t.to_string()),
            "FilterCriteria": m.filter_criteria().map(|f| f.filters().iter()
                .filter_map(|f| f.pattern()).collect::<Vec<_>>()),
            "OnFailureDestination": m.destination_config()
                .and_then(|d| d.on_failure()).and_then(|f| f.destination()),
        });
        Ok(Detail {
            fields,
            raw: format!("{m:#?}"),
        })
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .list_event_source_mappings()
        .max_items(100)
        .set_marker(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.event_source_mappings().iter().map(row).collect(),
        next: out.next_marker,
    })
}

fn row(m: &EventSourceMappingConfiguration) -> Row {
    let state = m.state().unwrap_or("-");
    let tone = match state {
        "Enabled" => Tone::Good,
        "Disabled" => Tone::Dim,
        _ => Tone::Warn,
    };
    Row::new(
        m.uuid().unwrap_or_default(),
        vec![
            or_dash(m.event_source_arn().map(arn_name)).into(),
            or_dash(m.function_arn().map(arn_name)).into(),
            Cell::toned(state, tone),
            m.batch_size()
                .map(|b| b.to_string())
                .unwrap_or_default()
                .into(),
            age_of(m.last_modified().map(|t| t.secs())).into(),
            or_dash(m.uuid()).into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_lambda::operation::list_event_source_mappings::ListEventSourceMappingsOutput;
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn mappings_show_source_and_function_names() {
        let rule = mock!(Client::list_event_source_mappings).then_output(|| {
            ListEventSourceMappingsOutput::builder()
                .event_source_mappings(
                    EventSourceMappingConfiguration::builder()
                        .uuid("u-1")
                        .event_source_arn("arn:aws:sqs:eu-west-1:1:jobs")
                        .function_arn("arn:aws:lambda:eu-west-1:1:function:worker")
                        .state("Enabled")
                        .batch_size(10)
                        .build(),
                )
                .build()
        });
        let client = mock_client!(aws_sdk_lambda, [&rule]);
        let page = list(&client, &ListQuery::default()).await.unwrap();
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(cells[..4], ["jobs", "worker", "Enabled", "10"]);
        assert_eq!(page.rows[0].cells[2].tone, Tone::Good);
        assert_eq!(page.next, None);
    }
}
