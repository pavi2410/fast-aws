//! `logs:insights`: a Logs Insights query over one log group, asked with `I`.
//!
//! Insights is billed per GB scanned, so results are never re-run on their own
//! (only with `r`), and the title says so.

use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_cloudwatchlogs::Client;
use aws_sdk_cloudwatchlogs::types::{QueryStatus, ResultField};
use fast_aws_core::fmt::bytes;
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ErrorKind, ListQuery, Page, Parent, Query, ResourceKind,
    Row, Scope, Tone, kind_info,
};
use serde_json::{Map, Value, json};

pub const PROMPT: &str = "Insights query (optionally start with -15m/-6h/-7d; default last hour)";

/// How long to wait for a query before giving up on it.
const TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_secs(1);
/// Separates a result row's ID parts.
const SEP: char = '\u{1f}';

pub struct Insights;

const COLUMNS: &[Column] = &[Column::fixed("@timestamp", 23), Column::fill("RESULT", 1)];

#[async_trait]
impl ResourceKind for Insights {
    kind_info! {
        id: "logs:insights",
        title: "Logs Insights (billed per GB scanned)",
        aliases: ["insights"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    /// Each run is billed, so a cached result is shown as it is.
    fn refresh_on_open(&self) -> bool {
        false
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("log group"))?;
        let (group, input) = Query::split(&parent.id);
        let now = fast_aws_core::fmt::now_secs();
        let (since, text) = parse(input);
        run(
            &Client::new(ctx.config()),
            group,
            text,
            now - since,
            now,
            POLL,
        )
        .await
    }

    /// The row holds its fields; no request.
    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        Ok(describe_result(id))
    }
}

/// A result's fields, from its row ID.
fn describe_result(id: &str) -> Detail {
    let fields = id
        .split_once(SEP)
        .and_then(|(_, json)| serde_json::from_str(json).ok())
        .unwrap_or(Value::Null);
    Detail {
        raw: format!("{fields:#}"),
        fields,
    }
}

/// `"-6h fields @message"` → (6 hours in seconds, `"fields @message"`).
pub(crate) fn parse(input: &str) -> (i64, &str) {
    let input = input.trim();
    let Some((first, rest)) = input.split_once(' ') else {
        return (3_600, input);
    };
    let Some(span) = first.strip_prefix('-') else {
        return (3_600, input);
    };
    let (number, unit) = span.split_at(span.len().saturating_sub(1));
    let seconds = match (number.parse::<i64>(), unit) {
        (Ok(n), "m") => n * 60,
        (Ok(n), "h") => n * 3_600,
        (Ok(n), "d") => n * 86_400,
        _ => return (3_600, input),
    };
    (seconds, rest.trim())
}

async fn run(
    client: &Client,
    group: &str,
    text: &str,
    start: i64,
    end: i64,
    poll: Duration,
) -> Result<Page, AwsError> {
    let started = client
        .start_query()
        .log_group_name(group)
        .query_string(text)
        .start_time(start)
        .end_time(end)
        .limit(1000)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let id = started.query_id().unwrap_or_default().to_owned();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let out = client
            .get_query_results()
            .query_id(&id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        match out.status() {
            Some(QueryStatus::Complete) => {
                let mut rows = vec![summary(&out)];
                rows.extend(out.results().iter().map(|r| row(r)));
                return Ok(Page { rows, next: None });
            }
            Some(QueryStatus::Failed | QueryStatus::Cancelled | QueryStatus::Timeout) => {
                let status = out.status().map(|s| s.as_str()).unwrap_or_default();
                return Err(AwsError::new(
                    ErrorKind::Other,
                    None,
                    format!("query {status}"),
                ));
            }
            _ if tokio::time::Instant::now() >= deadline => {
                // Stop it so it doesn't keep scanning (and billing) unseen.
                let _ = client.stop_query().query_id(&id).send().await;
                return Err(AwsError::new(
                    ErrorKind::Other,
                    None,
                    "query took over 2 minutes; stopped",
                ));
            }
            _ => tokio::time::sleep(poll).await,
        }
    }
}

fn summary(
    out: &aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput,
) -> Row {
    let text = match out.statistics() {
        Some(s) => format!(
            "{} matched of {} records · {} scanned",
            s.records_matched(),
            s.records_scanned(),
            bytes(s.bytes_scanned() as i64)
        ),
        None => format!("{} results", out.results().len()),
    };
    Row::new(
        format!("summary{SEP}null"),
        vec![Cell::toned("", Tone::Dim), Cell::toned(text, Tone::Dim)],
    )
}

fn row(fields: &[ResultField]) -> Row {
    let get = |name: &str| {
        fields
            .iter()
            .find(|f| f.field() == Some(name))
            .and_then(|f| f.value())
    };
    let time = get("@timestamp").unwrap_or_default();
    // Everything but the timestamp and Insights' internal @ptr, as field=value.
    let rest: Vec<String> = fields
        .iter()
        .filter(|f| !matches!(f.field(), Some("@timestamp" | "@ptr")))
        .map(|f| match f.field() {
            Some("@message") => f.value().unwrap_or_default().trim_end().to_owned(),
            _ => format!(
                "{}={}",
                f.field().unwrap_or_default(),
                f.value().unwrap_or_default()
            ),
        })
        .collect();
    let one_line: String = rest
        .join("  ")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let map: Map<String, Value> = fields
        .iter()
        .filter(|f| f.field() != Some("@ptr"))
        .map(|f| {
            let value = f.value().unwrap_or_default();
            // JSON messages are shown as a tree.
            let parsed = serde_json::from_str::<Value>(value)
                .ok()
                .filter(Value::is_object);
            (
                f.field().unwrap_or_default().to_owned(),
                parsed.unwrap_or_else(|| json!(value)),
            )
        })
        .collect();
    Row::new(
        format!("{time}{SEP}{}", Value::Object(map)),
        vec![time.into(), one_line.into()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput;
    use aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput;
    use aws_sdk_cloudwatchlogs::types::QueryStatistics;
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    #[test]
    fn a_leading_span_sets_the_time_range() {
        assert_eq!(parse("fields @message"), (3_600, "fields @message"));
        assert_eq!(parse("-6h fields @message"), (6 * 3_600, "fields @message"));
        assert_eq!(parse("-15m stats count()"), (900, "stats count()"));
        assert_eq!(parse("-7d filter x"), (7 * 86_400, "filter x"));
        assert_eq!(parse("-x filter"), (3_600, "-x filter"));
    }

    fn field(name: &str, value: &str) -> ResultField {
        ResultField::builder().field(name).value(value).build()
    }

    #[tokio::test]
    async fn runs_the_query_and_lists_results_after_a_summary() {
        let start = mock!(Client::start_query)
            .match_requests(|r| {
                r.log_group_name() == Some("/aws/lambda/thumbs")
                    && r.query_string() == Some("fields @message")
                    && r.start_time() == Some(100)
                    && r.end_time() == Some(200)
            })
            .then_output(|| StartQueryOutput::builder().query_id("q1").build());
        let results = mock!(Client::get_query_results)
            .match_requests(|r| r.query_id() == Some("q1"))
            .then_output(|| {
                GetQueryResultsOutput::builder()
                    .status(QueryStatus::Complete)
                    .statistics(
                        QueryStatistics::builder()
                            .records_matched(1.0)
                            .records_scanned(50.0)
                            .bytes_scanned(2048.0)
                            .build(),
                    )
                    .results(vec![
                        field("@timestamp", "2026-10-13 09:00:00.000"),
                        field("@message", "{\"level\":\"error\"}\n"),
                        field("@ptr", "abc"),
                    ])
                    .build()
            });
        let client = mock_client!(
            aws_sdk_cloudwatchlogs,
            RuleMode::MatchAny,
            [&start, &results]
        );

        let page = run(
            &client,
            "/aws/lambda/thumbs",
            "fields @message",
            100,
            200,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(
            page.rows[0].cells[1].text,
            "1 matched of 50 records · 2.0 KiB scanned"
        );
        assert_eq!(page.rows[1].cells[0].text, "2026-10-13 09:00:00.000");
        assert_eq!(page.rows[1].cells[1].text, "{\"level\":\"error\"}");
        let d = describe_result(&page.rows[1].id);
        assert_eq!(d.fields["@message"]["level"], "error");
    }

    #[tokio::test]
    async fn a_failed_query_is_an_error() {
        let start = mock!(Client::start_query)
            .then_output(|| StartQueryOutput::builder().query_id("q").build());
        let results = mock!(Client::get_query_results).then_output(|| {
            GetQueryResultsOutput::builder()
                .status(QueryStatus::Failed)
                .build()
        });
        let client = mock_client!(
            aws_sdk_cloudwatchlogs,
            RuleMode::MatchAny,
            [&start, &results]
        );
        let err = run(&client, "g", "bad", 0, 1, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(err.message, "query Failed");
    }
}
