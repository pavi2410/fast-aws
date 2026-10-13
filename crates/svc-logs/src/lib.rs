//! CloudWatch Logs resource kinds: log groups → streams → events.
//!
//! This is a plain paged view. Live tail and Logs Insights are later phases
//! (both are billed per use).

mod insights;

use async_trait::async_trait;
use aws_sdk_cloudwatchlogs::Client;
use aws_sdk_cloudwatchlogs::types::{LogGroup, LogStream, OrderBy, OutputLogEvent};
use fast_aws_core::fmt::{age_of, bytes, or_dash, utc_millis};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

/// Registers every CloudWatch Logs resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Groups);
    registry.register(Streams);
    registry.register(Events);
    registry.register(insights::Insights);
}

/// Separates group and stream in an event list's parent ID. Neither name can
/// contain a newline.
const SEP: char = '\n';
/// Separates timestamp and message in an event row's ID.
const EVENT_SEP: char = '\u{1f}';

fn millis_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.map(|ms| ms / 1000)
}

pub struct Groups;

const GROUP_COLUMNS: &[Column] = &[
    Column::fill("NAME", 4),
    Column::fixed("RETENTION", 9),
    Column::fixed("STORED", 10),
    Column::fixed("CLASS", 18),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Groups {
    kind_info! {
        id: "logs:group",
        title: "Log Groups",
        aliases: ["logs", "lg", "loggroups"],
        scope: Scope::Regional,
        columns: GROUP_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_groups(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_log_groups()
            .log_group_name_prefix(id)
            .limit(50)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let g = out
            .log_groups()
            .iter()
            .find(|g| g.log_group_name() == Some(id))
            .ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "LogGroupName": g.log_group_name(),
            "Arn": g.log_group_arn(),
            "RetentionInDays": g.retention_in_days(),
            "StoredBytes": g.stored_bytes().map(bytes),
            "LogGroupClass": g.log_group_class().map(|c| c.as_str()),
            "KmsKeyId": g.kms_key_id(),
            "MetricFilterCount": g.metric_filter_count(),
            "DataProtectionStatus": g.data_protection_status().map(|s| s.as_str()),
            "CreationTime": g.creation_time().map(utc_millis),
        });
        Ok(Detail {
            fields,
            raw: format!("{g:#?}"),
        })
    }
}

async fn list_groups(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .describe_log_groups()
        .limit(50)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out
            .log_groups()
            .iter()
            .map(|g| {
                let name = g.log_group_name().unwrap_or_default();
                source(group_row(g)).query('I', insights::PROMPT, "logs:insights", name)
            })
            .collect(),
        next: out.next_token,
    })
}

fn group_row(g: &LogGroup) -> Row {
    let name = g.log_group_name().unwrap_or_default();
    let retention = match g.retention_in_days() {
        Some(days) => Cell::new(format!("{days}d")),
        // Logs kept forever keep costing; worth seeing at a glance.
        None => Cell::toned("never", Tone::Warn),
    };
    Row::new(
        name,
        vec![
            name.into(),
            retention,
            g.stored_bytes().map(bytes).unwrap_or_default().into(),
            or_dash(g.log_group_class().map(|c| c.as_str())).into(),
            age_of(millis_to_secs(g.creation_time())).into(),
        ],
    )
    .drill(
        "logs:stream",
        Parent {
            id: name.to_owned(),
            label: name.to_owned(),
            region: None,
        },
    )
}

pub struct Streams;

const STREAM_COLUMNS: &[Column] = &[
    Column::fill("NAME", 4),
    Column::fixed("LAST EVENT", 19),
    Column::fixed("FIRST EVENT", 19),
];

#[async_trait]
impl ResourceKind for Streams {
    kind_info! {
        id: "logs:stream",
        title: "Log Streams",
        aliases: ["streams"],
        scope: Scope::Regional,
        columns: STREAM_COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let group = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("log group"))?;
        list_streams(&Client::new(ctx.config()), &group.id, query.next.clone()).await
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        id: &str,
    ) -> Result<Detail, AwsError> {
        let group = parent.ok_or_else(|| AwsError::not_found("log group"))?;
        let out = Client::new(ctx.config())
            .describe_log_streams()
            .log_group_name(&group.id)
            .log_stream_name_prefix(id)
            .limit(50)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let s = out
            .log_streams()
            .iter()
            .find(|s| s.log_stream_name() == Some(id))
            .ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "LogGroupName": group.id,
            "LogStreamName": s.log_stream_name(),
            "Arn": s.arn(),
            "CreationTime": s.creation_time().map(utc_millis),
            "FirstEventTimestamp": s.first_event_timestamp().map(utc_millis),
            "LastEventTimestamp": s.last_event_timestamp().map(utc_millis),
            "LastIngestionTime": s.last_ingestion_time().map(utc_millis),
        });
        Ok(Detail {
            fields,
            raw: format!("{s:#?}"),
        })
    }
}

/// Streams with the most recent activity first, which is nearly always the one
/// wanted.
async fn list_streams(
    client: &Client,
    group: &str,
    next: Option<String>,
) -> Result<Page, AwsError> {
    let out = client
        .describe_log_streams()
        .log_group_name(group)
        .order_by(OrderBy::LastEventTime)
        .descending(true)
        .limit(50)
        .set_next_token(next)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .log_streams()
        .iter()
        .map(|s| stream_row(group, s))
        .collect();
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

fn stream_row(group: &str, s: &LogStream) -> Row {
    let name = s.log_stream_name().unwrap_or_default();
    let parent = Parent {
        id: format!("{group}{SEP}{name}"),
        label: name.to_owned(),
        region: None,
    };
    Row::new(
        name,
        vec![
            name.into(),
            or_dash(s.last_event_timestamp().map(utc_millis).as_deref()).into(),
            or_dash(s.first_event_timestamp().map(utc_millis).as_deref()).into(),
        ],
    )
    .drill("logs:event", parent)
}

pub struct Events;

const EVENT_COLUMNS: &[Column] = &[Column::fixed("TIME (UTC)", 19), Column::fill("MESSAGE", 1)];

#[async_trait]
impl ResourceKind for Events {
    kind_info! {
        id: "logs:event",
        title: "Log Events",
        aliases: ["events"],
        scope: Scope::Regional,
        columns: EVENT_COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("log stream"))?;
        let (group, stream) = parent.id.split_once(SEP).unwrap_or((&parent.id, ""));
        list_events(
            &Client::new(ctx.config()),
            group,
            stream,
            query.next.clone(),
        )
        .await
    }

    /// Everything is in the row already; no call is made.
    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        Ok(describe_event(id))
    }
}

/// An event from its row ID. JSON messages are shown as a tree.
fn describe_event(id: &str) -> Detail {
    let (time, message) = id.split_once(EVENT_SEP).unwrap_or(("", id));
    let parsed = serde_json::from_str::<Value>(message).unwrap_or_else(|_| json!(message));
    let fields = json!({ "Timestamp": time, "Message": parsed });
    Detail {
        fields,
        raw: message.to_owned(),
    }
}

/// The newest events first; each further page goes back in time.
async fn list_events(
    client: &Client,
    group: &str,
    stream: &str,
    next: Option<String>,
) -> Result<Page, AwsError> {
    let out = client
        .get_log_events()
        .log_group_name(group)
        .log_stream_name(stream)
        .start_from_head(false)
        .limit(200)
        .set_next_token(next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out.events().iter().rev().map(event_row).collect();
    // The API hands back the token it was given once there is nothing older.
    let older = out.next_backward_token.filter(|t| Some(t) != next.as_ref());
    Ok(Page { rows, next: older })
}

fn event_row(e: &OutputLogEvent) -> Row {
    let time = e.timestamp().map(utc_millis).unwrap_or_default();
    let message = e.message().unwrap_or_default().trim_end();
    let one_line: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    Row::new(
        format!("{time}{EVENT_SEP}{message}"),
        vec![time.as_str().into(), one_line.into()],
    )
}

/// What `g` offers from a log group: the Lambda function that writes to it,
/// when the name follows Lambda's convention.
fn source(row: Row) -> Row {
    match row.id.strip_prefix("/aws/lambda/").map(str::to_owned) {
        Some(function) => row.relate(format!("function {function}"), "lambda:function", &function),
        None => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatchlogs::operation::describe_log_groups::DescribeLogGroupsOutput;
    use aws_sdk_cloudwatchlogs::operation::get_log_events::GetLogEventsOutput;
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn groups_flag_unlimited_retention_and_drill_into_streams() {
        let rule = mock!(Client::describe_log_groups)
            .match_requests(|r| r.next_token() == Some("t1"))
            .then_output(|| {
                DescribeLogGroupsOutput::builder()
                    .log_groups(
                        LogGroup::builder()
                            .log_group_name("/aws/lambda/thumbs")
                            .stored_bytes(2048)
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudwatchlogs, [&rule]);
        let page = list_groups(
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
        assert_eq!(row.cells[1].text, "never");
        assert_eq!(row.cells[1].tone, Tone::Warn);
        assert_eq!(row.drill.as_ref().unwrap().parent.id, "/aws/lambda/thumbs");
    }

    #[test]
    fn lambda_log_groups_lead_to_their_function() {
        let lambda = source(Row::new("/aws/lambda/thumbs", vec![]));
        assert_eq!(lambda.relations[0].filter.as_deref(), Some("thumbs"));
        assert!(source(Row::new("/ecs/web", vec![])).relations.is_empty());
    }

    #[test]
    fn stream_rows_drill_into_their_events() {
        let s = LogStream::builder()
            .log_stream_name("2026/10/13/[$LATEST]abc")
            .build();
        let drill = stream_row("/aws/lambda/thumbs", &s).drill.unwrap();
        assert_eq!(drill.kind, "logs:event");
        assert_eq!(
            drill.parent.id,
            "/aws/lambda/thumbs\n2026/10/13/[$LATEST]abc"
        );
    }

    #[tokio::test]
    async fn events_are_newest_first_and_stop_when_the_token_repeats() {
        let rule = mock!(Client::get_log_events)
            .match_requests(|r| {
                r.log_group_name() == Some("g")
                    && r.log_stream_name() == Some("s")
                    && r.start_from_head() == Some(false)
                    && r.next_token() == Some("b1")
            })
            .then_output(|| {
                GetLogEventsOutput::builder()
                    .events(
                        OutputLogEvent::builder()
                            .timestamp(0)
                            .message("first\n")
                            .build(),
                    )
                    .events(
                        OutputLogEvent::builder()
                            .timestamp(1000)
                            .message("second\tx")
                            .build(),
                    )
                    .next_backward_token("b1")
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudwatchlogs, [&rule]);
        let page = list_events(&client, "g", "s", Some("b1".into()))
            .await
            .unwrap();
        let messages: Vec<_> = page.rows.iter().map(|r| r.cells[1].text.as_str()).collect();
        assert_eq!(messages, ["second x", "first"]);
        assert_eq!(page.rows[0].cells[0].text, "1970-01-01 00:00:01");
        assert_eq!(page.next, None);
    }

    #[test]
    fn json_messages_describe_as_a_tree() {
        let id = format!("2026-10-13 09:00:00{EVENT_SEP}{{\"level\":\"error\",\"code\":7}}");
        let d = describe_event(&id);
        assert_eq!(d.fields["Message"]["level"], "error");
        assert_eq!(d.fields["Timestamp"], "2026-10-13 09:00:00");
        assert_eq!(describe_event("plain").fields["Message"], "plain");
    }
}
