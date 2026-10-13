//! EventBridge: event buses and rules, and EventBridge Scheduler schedules.
//!
//! Pipes are out of scope for now (see `docs/plan/services/eventbridge.md`).

use async_trait::async_trait;
use aws_sdk_eventbridge::Client as EventsClient;
use aws_sdk_eventbridge::types::{EventBus, Rule, RuleState};
use aws_sdk_scheduler::Client as SchedulerClient;
use aws_sdk_scheduler::types::{ScheduleState, ScheduleSummary};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

/// Registers every EventBridge resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Buses);
    registry.register(Rules);
    registry.register(Schedules);
}

const DEFAULT_BUS: &str = "default";

/// The last part of an ARN, for compact cells.
fn arn_name(arn: &str) -> &str {
    arn.rsplit([':', '/']).next().unwrap_or(arn)
}

/// What fires a rule, in one line: its schedule, or the pattern's sources and
/// detail types (`aws.ec2: EC2 Instance State-change Notification`).
fn trigger(rule_schedule: Option<&str>, pattern: Option<&str>) -> String {
    if let Some(schedule) = rule_schedule {
        return schedule.to_owned();
    }
    let Some(pattern) = pattern.and_then(|p| serde_json::from_str::<Value>(p).ok()) else {
        return "-".into();
    };
    let list = |key: &str| match &pattern[key] {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    match (list("source"), list("detail-type")) {
        (s, d) if s.is_empty() && d.is_empty() => "pattern".into(),
        (s, d) if d.is_empty() => s,
        (s, d) if s.is_empty() => d,
        (s, d) => format!("{s}: {d}"),
    }
}

fn state_cell(enabled: bool, text: &str) -> Cell {
    if enabled {
        Cell::toned(text, Tone::Good)
    } else {
        Cell::toned(text, Tone::Dim)
    }
}

pub struct Buses;

const BUS_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fill("DESCRIPTION", 3),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Buses {
    kind_info! {
        id: "events:bus",
        title: "EventBridge Buses",
        aliases: ["eb", "bus", "buses", "eventbridge"],
        scope: Scope::Regional,
        columns: BUS_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = EventsClient::new(ctx.config())
            .list_event_buses()
            .limit(100)
            .set_next_token(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out.event_buses().iter().map(bus_row).collect(),
            next: out.next_token,
        })
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        _: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        let b = EventsClient::new(ctx.config())
            .describe_event_bus()
            .name(name)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let policy = b
            .policy()
            .and_then(|p| serde_json::from_str::<Value>(p).ok());
        let fields = json!({
            "Name": b.name(),
            "Arn": b.arn(),
            "Description": b.description(),
            "KmsKeyIdentifier": b.kms_key_identifier(),
            "DeadLetterQueue": b.dead_letter_config().and_then(|d| d.arn()),
            "Policy": policy,
            "CreationTime": b.creation_time().map(|t| t.to_string()),
        });
        Ok(Detail {
            fields,
            raw: format!("{b:#?}"),
        })
    }
}

fn bus_row(b: &EventBus) -> Row {
    let name = b.name().unwrap_or_default();
    Row::new(
        name,
        vec![
            name.into(),
            or_dash(b.description()).into(),
            age_of(b.creation_time().map(|t| t.secs())).into(),
        ],
    )
    .drill(
        "events:rule",
        Parent {
            id: name.to_owned(),
            label: name.to_owned(),
            region: None,
        },
    )
}

pub struct Rules;

const RULE_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("BUS", 12),
    Column::fixed("STATE", 9),
    Column::fill("TRIGGER", 4),
    Column::fixed("MANAGED BY", 14),
];

#[async_trait]
impl ResourceKind for Rules {
    kind_info! {
        id: "events:rule",
        title: "EventBridge Rules",
        aliases: ["rule", "rules"],
        scope: Scope::Regional,
        columns: RULE_COLUMNS,
    }

    /// The rules of the bus drilled into from, or of the default bus.
    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let bus = query.parent.as_ref().map_or(DEFAULT_BUS, |p| p.id.as_str());
        list_rules(&EventsClient::new(ctx.config()), bus, query.next.clone()).await
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        let bus = parent.map_or(DEFAULT_BUS, |p| p.id.as_str());
        let client = EventsClient::new(ctx.config());
        let (rule, targets) = tokio::join!(
            client.describe_rule().name(name).event_bus_name(bus).send(),
            client
                .list_targets_by_rule()
                .rule(name)
                .event_bus_name(bus)
                .send(),
        );
        let r = rule.map_err(AwsError::from_sdk)?;
        let targets = match targets {
            Ok(t) => json!(
                t.targets()
                    .iter()
                    .map(|t| json!({
                        "Id": t.id(),
                        "Arn": t.arn(),
                        "RoleArn": t.role_arn(),
                        "DeadLetterQueue": t.dead_letter_config().and_then(|d| d.arn()),
                    }))
                    .collect::<Vec<_>>()
            ),
            Err(e) => json!(format!("⚠ {}", AwsError::from_sdk(e))),
        };
        let pattern = r
            .event_pattern()
            .and_then(|p| serde_json::from_str::<Value>(p).ok());
        let fields = json!({
            "Name": r.name(),
            "Arn": r.arn(),
            "EventBus": r.event_bus_name(),
            "State": r.state().map(|s| s.as_str()),
            "Description": r.description(),
            "ScheduleExpression": r.schedule_expression(),
            "EventPattern": pattern,
            "Targets": targets,
            "RoleArn": r.role_arn(),
            "ManagedBy": r.managed_by(),
            "CreatedBy": r.created_by(),
        });
        Ok(Detail {
            fields,
            raw: format!("{r:#?}"),
        })
    }
}

async fn list_rules(
    client: &EventsClient,
    bus: &str,
    next: Option<String>,
) -> Result<Page, AwsError> {
    let out = client
        .list_rules()
        .event_bus_name(bus)
        .limit(100)
        .set_next_token(next)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.rules().iter().map(rule_row).collect(),
        next: out.next_token,
    })
}

fn rule_row(r: &Rule) -> Row {
    let state = r.state().map(|s| s.as_str()).unwrap_or("-");
    Row::new(
        r.name().unwrap_or_default(),
        vec![
            or_dash(r.name()).into(),
            or_dash(r.event_bus_name()).into(),
            state_cell(r.state() == Some(&RuleState::Enabled), state),
            trigger(r.schedule_expression(), r.event_pattern()).into(),
            // Rules created by AWS services can't be edited.
            or_dash(r.managed_by()).into(),
        ],
    )
}

pub struct Schedules;

const SCHEDULE_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("GROUP", 14),
    Column::fixed("STATE", 9),
    Column::fill("TARGET", 3),
    Column::fixed("MODIFIED", 8),
];

#[async_trait]
impl ResourceKind for Schedules {
    kind_info! {
        id: "scheduler:schedule",
        title: "EventBridge Schedules",
        aliases: ["sched", "schedule", "schedules", "scheduler"],
        scope: Scope::Regional,
        columns: SCHEDULE_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_schedules(&SchedulerClient::new(ctx.config()), query.next.clone()).await
    }

    /// Row IDs are `group/name`; the expression and time zone are only in
    /// GetSchedule.
    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let (group, name) = id.split_once('/').unwrap_or((DEFAULT_BUS, id));
        let s = SchedulerClient::new(ctx.config())
            .get_schedule()
            .group_name(group)
            .name(name)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let fields = json!({
            "Name": s.name(),
            "Group": s.group_name(),
            "Arn": s.arn(),
            "State": s.state().map(|s| s.as_str()),
            "ScheduleExpression": s.schedule_expression(),
            "Timezone": s.schedule_expression_timezone(),
            "StartDate": s.start_date().map(|d| d.to_string()),
            "EndDate": s.end_date().map(|d| d.to_string()),
            "FlexibleTimeWindow": s.flexible_time_window().map(|f| json!({
                "Mode": f.mode().as_str(),
                "MaximumWindowInMinutes": f.maximum_window_in_minutes(),
            })),
            "Target": s.target().map(|t| json!({
                "Arn": t.arn(),
                "RoleArn": t.role_arn(),
                "DeadLetterQueue": t.dead_letter_config().and_then(|d| d.arn()),
            })),
            "ActionAfterCompletion": s.action_after_completion().map(|a| a.as_str()),
            "Description": s.description(),
            "LastModified": s.last_modification_date().map(|d| d.to_string()),
        });
        Ok(Detail {
            fields,
            raw: format!("{s:#?}"),
        })
    }
}

async fn list_schedules(client: &SchedulerClient, next: Option<String>) -> Result<Page, AwsError> {
    let out = client
        .list_schedules()
        .max_results(100)
        .set_next_token(next)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out
            .schedules()
            .iter()
            .map(|s| schedule_target(schedule_row(s), s))
            .collect(),
        next: out.next_token,
    })
}

fn schedule_row(s: &ScheduleSummary) -> Row {
    let group = s.group_name().unwrap_or(DEFAULT_BUS);
    let name = s.name().unwrap_or_default();
    let state = s.state().map(|s| s.as_str()).unwrap_or("-");
    Row::new(
        format!("{group}/{name}"),
        vec![
            name.into(),
            group.into(),
            state_cell(s.state() == Some(&ScheduleState::Enabled), state),
            or_dash(s.target().map(|t| arn_name(t.arn()))).into(),
            age_of(s.last_modification_date().map(|d| d.secs())).into(),
        ],
    )
}

/// What `g` offers from a schedule: its Lambda target.
fn schedule_target(row: Row, s: &ScheduleSummary) -> Row {
    let arn = s
        .target()
        .map(|t| t.arn())
        .and_then(fast_aws_core::arn::Arn::parse);
    match arn {
        Some(a) if a.service == "lambda" => row.relate(
            format!("function {}", a.name()),
            "lambda:function",
            a.name(),
        ),
        _ => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_eventbridge::operation::list_rules::ListRulesOutput;
    use aws_sdk_scheduler::operation::list_schedules::ListSchedulesOutput;
    use aws_sdk_scheduler::types::TargetSummary;
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn triggers_read_naturally() {
        assert_eq!(trigger(Some("rate(5 minutes)"), None), "rate(5 minutes)");
        let pattern =
            r#"{"source":["aws.ec2"],"detail-type":["EC2 Instance State-change Notification"]}"#;
        assert_eq!(
            trigger(None, Some(pattern)),
            "aws.ec2: EC2 Instance State-change Notification"
        );
        assert_eq!(trigger(None, Some(r#"{"source":["myapp"]}"#)), "myapp");
        assert_eq!(trigger(None, Some(r#"{"detail":{"x":[1]}}"#)), "pattern");
        assert_eq!(trigger(None, None), "-");
    }

    #[tokio::test]
    async fn rules_list_the_given_bus() {
        let rule = mock!(EventsClient::list_rules)
            .match_requests(|r| {
                r.event_bus_name() == Some("orders") && r.next_token() == Some("t1")
            })
            .then_output(|| {
                ListRulesOutput::builder()
                    .rules(
                        Rule::builder()
                            .name("nightly")
                            .event_bus_name("orders")
                            .state(RuleState::Disabled)
                            .schedule_expression("cron(0 2 * * ? *)")
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_eventbridge, [&rule]);
        let page = list_rules(&client, "orders", Some("t1".into()))
            .await
            .unwrap();
        assert_eq!(page.next.as_deref(), Some("t2"));
        assert_eq!(page.rows[0].cells[2].tone, Tone::Dim);
        assert_eq!(page.rows[0].cells[3].text, "cron(0 2 * * ? *)");
    }

    #[test]
    fn buses_drill_into_their_rules() {
        let bus = EventBus::builder().name("orders").build();
        let drill = bus_row(&bus).drill.unwrap();
        assert_eq!(
            (drill.kind, drill.parent.id.as_str()),
            ("events:rule", "orders")
        );
    }

    #[tokio::test]
    async fn schedules_are_identified_by_group_and_name() {
        let rule = mock!(SchedulerClient::list_schedules).then_output(|| {
            ListSchedulesOutput::builder()
                .schedules(
                    ScheduleSummary::builder()
                        .name("report")
                        .group_name("billing")
                        .state(ScheduleState::Enabled)
                        .target(
                            TargetSummary::builder()
                                .arn("arn:aws:lambda:x:1:function:make-report")
                                .build()
                                .unwrap(),
                        )
                        .build(),
                )
                .build()
                .unwrap()
        });
        let client = mock_client!(aws_sdk_scheduler, [&rule]);
        let page = list_schedules(&client, None).await.unwrap();
        assert_eq!(page.rows[0].id, "billing/report");
        assert_eq!(
            page.rows[0].relations[0].filter.as_deref(),
            Some("make-report")
        );
        assert_eq!(page.rows[0].cells[3].text, "make-report");
        assert_eq!(page.rows[0].cells[2].tone, Tone::Good);
    }
}
