//! CloudWatch: alarms (metric and composite).
//!
//! Metric charts and sparklines come later: `GetMetricData` is billed per metric
//! requested, so it needs the visible-rows batching from the plan first.

use async_trait::async_trait;
use aws_sdk_cloudwatch::Client;
use aws_sdk_cloudwatch::types::{
    AlarmType, ComparisonOperator, CompositeAlarm, Dimension, MetricAlarm, StateValue,
};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

/// Registers every CloudWatch resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Alarms);
}

pub struct Alarms;

const COLUMNS: &[Column] = &[
    Column::fixed("STATE", 17),
    Column::fill("NAME", 3),
    Column::fill("METRIC", 3),
    Column::fixed("CONDITION", 16),
    Column::fixed("CHANGED", 7),
    Column::fixed("ACTIONS", 7),
];

#[async_trait]
impl ResourceKind for Alarms {
    kind_info! {
        id: "cloudwatch:alarm",
        title: "CloudWatch Alarms",
        aliases: ["alarms", "alarm", "cw"],
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
        .describe_alarms()
        .alarm_types(AlarmType::MetricAlarm)
        .alarm_types(AlarmType::CompositeAlarm)
        .max_records(100)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let mut rows: Vec<(u8, Row)> = out
        .metric_alarms()
        .iter()
        .map(|a| (rank(a.state_value()), metric_row(a)))
        .chain(
            out.composite_alarms()
                .iter()
                .map(|a| (rank(a.state_value()), composite_row(a))),
        )
        .collect();
    // Firing alarms first (within each page); the order is otherwise by name.
    rows.sort_by_key(|(rank, _)| *rank);
    Ok(Page {
        rows: rows.into_iter().map(|(_, r)| r).collect(),
        next: out.next_token,
    })
}

fn rank(state: Option<&StateValue>) -> u8 {
    match state {
        Some(StateValue::Alarm) => 0,
        Some(StateValue::InsufficientData) => 1,
        _ => 2,
    }
}

fn state_cell(state: Option<&StateValue>) -> Cell {
    match state {
        Some(StateValue::Alarm) => Cell::toned("● ALARM", Tone::Bad),
        Some(StateValue::Ok) => Cell::toned("● OK", Tone::Good),
        Some(StateValue::InsufficientData) => Cell::toned("○ INSUFFICIENT", Tone::Dim),
        _ => Cell::toned("?", Tone::Dim),
    }
}

fn actions_cell(enabled: Option<bool>) -> Cell {
    match enabled {
        Some(false) => Cell::toned("muted", Tone::Warn),
        _ => Cell::new("on"),
    }
}

/// `AWS/Lambda Errors FunctionName=thumbs`.
fn metric_label(a: &MetricAlarm) -> String {
    if !a.metrics().is_empty() {
        return "metric math".into();
    }
    let dims: Vec<String> = a.dimensions().iter().map(dimension).collect();
    let base = format!("{} {}", or_dash(a.namespace()), or_dash(a.metric_name()));
    if dims.is_empty() {
        base
    } else {
        format!("{base} {}", dims.join(","))
    }
}

fn dimension(d: &Dimension) -> String {
    format!("{}={}", or_dash(d.name()), or_dash(d.value()))
}

/// `> 5 for 3×60s`.
fn condition(a: &MetricAlarm) -> String {
    let op = match a.comparison_operator() {
        Some(ComparisonOperator::GreaterThanThreshold) => ">",
        Some(ComparisonOperator::GreaterThanOrEqualToThreshold) => "≥",
        Some(ComparisonOperator::LessThanThreshold) => "<",
        Some(ComparisonOperator::LessThanOrEqualToThreshold) => "≤",
        Some(_) => "anomaly",
        None => "?",
    };
    let threshold = a.threshold().map(|t| t.to_string()).unwrap_or_default();
    match (a.evaluation_periods(), a.period()) {
        (Some(n), Some(p)) => format!("{op} {threshold} for {n}×{p}s"),
        _ => format!("{op} {threshold}"),
    }
}

fn metric_row(a: &MetricAlarm) -> Row {
    Row::new(
        a.alarm_name().unwrap_or_default(),
        vec![
            state_cell(a.state_value()),
            or_dash(a.alarm_name()).into(),
            metric_label(a).into(),
            condition(a).into(),
            age_of(a.state_updated_timestamp().map(|t| t.secs())).into(),
            actions_cell(a.actions_enabled()),
        ],
    )
}

fn composite_row(a: &CompositeAlarm) -> Row {
    Row::new(
        a.alarm_name().unwrap_or_default(),
        vec![
            state_cell(a.state_value()),
            or_dash(a.alarm_name()).into(),
            "composite".into(),
            or_dash(a.alarm_rule()).into(),
            age_of(a.state_updated_timestamp().map(|t| t.secs())).into(),
            actions_cell(a.actions_enabled()),
        ],
    )
}

async fn describe(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let (alarms, history) = tokio::join!(
        client
            .describe_alarms()
            .alarm_names(name)
            .alarm_types(AlarmType::MetricAlarm)
            .alarm_types(AlarmType::CompositeAlarm)
            .send(),
        client
            .describe_alarm_history()
            .alarm_name(name)
            .max_records(10)
            .send(),
    );
    let alarms = alarms.map_err(AwsError::from_sdk)?;
    let history: Value = match history {
        Ok(h) => json!(
            h.alarm_history_items()
                .iter()
                .map(|i| json!({
                    "Time": i.timestamp().map(|t| t.to_string()),
                    "Type": i.history_item_type().map(|t| t.as_str()),
                    "Summary": i.history_summary(),
                }))
                .collect::<Vec<_>>()
        ),
        Err(e) => json!(format!("⚠ {}", AwsError::from_sdk(e))),
    };
    let fields = if let Some(a) = alarms.metric_alarms().first() {
        json!({
            "Name": a.alarm_name(),
            "Arn": a.alarm_arn(),
            "Description": a.alarm_description(),
            "State": a.state_value().map(|s| s.as_str()),
            "StateReason": a.state_reason(),
            "Metric": metric_label(a),
            "Statistic": a.statistic().map(|s| s.as_str()).or(a.extended_statistic()),
            "Condition": condition(a),
            "DatapointsToAlarm": a.datapoints_to_alarm(),
            "TreatMissingData": a.treat_missing_data(),
            "ActionsEnabled": a.actions_enabled(),
            "AlarmActions": a.alarm_actions(),
            "OkActions": a.ok_actions(),
            "InsufficientDataActions": a.insufficient_data_actions(),
            "RecentHistory": history,
        })
    } else if let Some(a) = alarms.composite_alarms().first() {
        json!({
            "Name": a.alarm_name(),
            "Arn": a.alarm_arn(),
            "Description": a.alarm_description(),
            "State": a.state_value().map(|s| s.as_str()),
            "StateReason": a.state_reason(),
            "Rule": a.alarm_rule(),
            "ActionsEnabled": a.actions_enabled(),
            "AlarmActions": a.alarm_actions(),
            "RecentHistory": history,
        })
    } else {
        return Err(AwsError::not_found(name));
    };
    Ok(Detail {
        raw: format!("{alarms:#?}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_smithy_mocks::{mock, mock_client};

    fn alarm(name: &str, state: StateValue) -> MetricAlarm {
        MetricAlarm::builder()
            .alarm_name(name)
            .state_value(state)
            .namespace("AWS/Lambda")
            .metric_name("Errors")
            .dimensions(
                Dimension::builder()
                    .name("FunctionName")
                    .value("thumbs")
                    .build(),
            )
            .comparison_operator(ComparisonOperator::GreaterThanThreshold)
            .threshold(5.0)
            .evaluation_periods(3)
            .period(60)
            .build()
    }

    #[tokio::test]
    async fn firing_alarms_come_first() {
        let rule = mock!(Client::describe_alarms)
            .match_requests(|r| r.alarm_types().len() == 2 && r.next_token() == Some("t1"))
            .then_output(|| {
                DescribeAlarmsOutput::builder()
                    .metric_alarms(alarm("quiet", StateValue::Ok))
                    .metric_alarms(alarm("loud", StateValue::Alarm))
                    .composite_alarms(
                        CompositeAlarm::builder()
                            .alarm_name("checkout-health")
                            .state_value(StateValue::InsufficientData)
                            .alarm_rule("ALARM(loud) OR ALARM(quiet)")
                            .actions_enabled(false)
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudwatch, [&rule]);
        let page = list(
            &client,
            &ListQuery {
                parent: None,
                next: Some("t1".into()),
            },
        )
        .await
        .unwrap();
        let names: Vec<_> = page.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(names, ["loud", "checkout-health", "quiet"]);
        let loud: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            loud,
            [
                "● ALARM",
                "loud",
                "AWS/Lambda Errors FunctionName=thumbs",
                "> 5 for 3×60s",
                "-",
                "on"
            ]
        );
        assert_eq!(page.rows[1].cells[5].text, "muted");
        assert_eq!(page.next.as_deref(), Some("t2"));
    }
}
