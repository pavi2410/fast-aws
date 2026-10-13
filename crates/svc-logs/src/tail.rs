//! `logs:tail`: a log group followed live, every stream at once. Opened from
//! `g` on a log group (or a Lambda function).
//!
//! Polls `FilterLogEvents` every [`EVERY`], from five minutes back, then from
//! just after the newest event seen. (`StartLiveTail` is billed per session
//! minute, so it isn't used.)
//!
//! `FilterLogEvents` is rate-limited per account and region, and the limit
//! can't be raised: 25/s in us-east-1, 10/s in most regions, 5/s in a few
//! (eu-central-1 among them). One poll every two seconds, of at most
//! [`PAGES_PER_POLL`] calls, leaves most of that to everything else.

use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_cloudwatchlogs::Client;
use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
use fast_aws_core::fmt::utc_millis;
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, kind_info,
};

use crate::EVENT_SEP;

const EVERY: Duration = Duration::from_secs(2);
const LOOKBACK_MS: i64 = 5 * 60 * 1000;
/// Result pages read per poll, at most; a busy group catches up over the
/// next polls instead of in one burst.
const PAGES_PER_POLL: usize = 2;

pub struct Tail;

const COLUMNS: &[Column] = &[
    Column::fixed("TIME (UTC)", 19),
    Column::fixed("STREAM", 24),
    Column::fill("MESSAGE", 1),
];

#[async_trait]
impl ResourceKind for Tail {
    kind_info! {
        id: "logs:tail",
        title: "Live Log Tail",
        aliases: ["tail"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    fn follow_every(&self) -> Option<Duration> {
        Some(EVERY)
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let group = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("log group"))?;
        let since = match query.next.as_deref().and_then(|t| t.parse().ok()) {
            Some(ms) => ms,
            None => fast_aws_core::fmt::now_secs() * 1000 - LOOKBACK_MS,
        };
        poll(&Client::new(ctx.config()), &group.id, since).await
    }

    /// The row holds its event; no request.
    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        id: &str,
    ) -> Result<Detail, AwsError> {
        // `<event id><SEP><time><SEP><message>`: drop the ID, then it reads like
        // a stream's event.
        let event = id.split_once(EVENT_SEP).map_or(id, |(_, rest)| rest);
        crate::Events.describe(ctx, parent, event).await
    }
}

/// Events since `since_ms`, oldest first; the next token is the millisecond
/// after the newest one (or `since_ms` again when there were none).
async fn poll(client: &Client, group: &str, since_ms: i64) -> Result<Page, AwsError> {
    let mut events: Vec<FilteredLogEvent> = Vec::new();
    let mut token = None;
    for _ in 0..PAGES_PER_POLL {
        let out = client
            .filter_log_events()
            .log_group_name(group)
            .start_time(since_ms)
            .set_next_token(token)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        events.extend(out.events().iter().cloned());
        token = out.next_token;
        if token.is_none() {
            break;
        }
    }
    events.sort_by_key(|e| e.timestamp().unwrap_or_default());
    let newest = events.iter().filter_map(|e| e.timestamp()).max();
    let next = newest.map_or(since_ms, |t| t + 1).to_string();
    Ok(Page {
        rows: events.iter().map(row).collect(),
        next: Some(next),
    })
}

fn row(e: &FilteredLogEvent) -> Row {
    let time = e.timestamp().map(utc_millis).unwrap_or_default();
    let message = e.message().unwrap_or_default().trim_end();
    let one_line: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let stream = e.log_stream_name().unwrap_or_default();
    // The last part of a stream name is the distinctive one ([$LATEST]abc…).
    let short = stream.rsplit('/').next().unwrap_or(stream);
    let id = format!(
        "{}{EVENT_SEP}{time}{EVENT_SEP}{message}",
        e.event_id().unwrap_or_default()
    );
    Row::new(
        id,
        vec![time.as_str().into(), short.into(), one_line.into()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_smithy_mocks::{mock, mock_client};

    fn event(id: &str, ms: i64, message: &str) -> FilteredLogEvent {
        FilteredLogEvent::builder()
            .event_id(id)
            .timestamp(ms)
            .message(message)
            .log_stream_name("2026/10/13/[$LATEST]abc")
            .build()
    }

    #[tokio::test]
    async fn polls_from_the_cursor_and_moves_it_past_the_newest_event() {
        let rule = mock!(Client::filter_log_events)
            .match_requests(|r| {
                r.log_group_name() == Some("/aws/lambda/thumbs") && r.start_time() == Some(5_000)
            })
            .then_output(|| {
                FilterLogEventsOutput::builder()
                    .events(event("b", 7_000, "second\n"))
                    .events(event("a", 6_000, "first"))
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudwatchlogs, [&rule]);
        let page = poll(&client, "/aws/lambda/thumbs", 5_000).await.unwrap();
        let messages: Vec<_> = page.rows.iter().map(|r| r.cells[2].text.as_str()).collect();
        assert_eq!(messages, ["first", "second"], "oldest first");
        assert_eq!(page.rows[0].cells[1].text, "[$LATEST]abc");
        assert_eq!(page.next.as_deref(), Some("7001"));
    }

    #[tokio::test]
    async fn a_quiet_poll_keeps_the_cursor() {
        let rule = mock!(Client::filter_log_events)
            .then_output(|| FilterLogEventsOutput::builder().build());
        let client = mock_client!(aws_sdk_cloudwatchlogs, [&rule]);
        let page = poll(&client, "g", 9_000).await.unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.next.as_deref(), Some("9000"));
    }

    #[test]
    fn rows_are_unique_by_event_id_and_describe_like_stream_events() {
        let r = row(&event("e1", 0, "{\"level\":\"warn\"}"));
        assert!(r.id.starts_with("e1"));
        let (_, rest) = r.id.split_once(EVENT_SEP).unwrap();
        assert_eq!(
            crate::describe_event(rest).fields["Message"]["level"],
            "warn"
        );
    }
}
