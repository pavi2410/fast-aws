//! Cost Explorer: month-to-date cost by service.
//!
//! Cost Explorer charges for every API request (about $0.01). Opening the view
//! makes exactly one request, nothing refreshes it automatically, and further
//! result pages are not fetched. The endpoint is in `us-east-1` only.

use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_costexplorer::Client;
use aws_sdk_costexplorer::types::{
    DateInterval, Granularity, GroupDefinition, GroupDefinitionType, ResultByTime,
};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

/// Registers every Cost Explorer resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(CostByService);
}

const METRIC: &str = "UnblendedCost";
/// Separates the parts of a row ID, which carries the numbers so describe
/// doesn't need another (billed) request.
const SEP: char = '\u{1f}';

pub struct CostByService;

const COLUMNS: &[Column] = &[
    Column::fill("SERVICE", 3),
    Column::fixed("MONTH TO DATE", 14),
    Column::fixed("SHARE", 6),
    Column::fill("", 2),
];

#[async_trait]
impl ResourceKind for CostByService {
    kind_info! {
        id: "cost:service",
        title: "Cost by Service (MTD · $0.01 per request)",
        aliases: ["cost", "costs", "billing"],
        scope: Scope::Pinned("us-east-1"),
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
        let (start, end) = month_to_date(fast_aws_core::fmt::now_secs());
        list(&Client::new(ctx.config()), &start, &end).await
    }

    /// No request: the row already holds the numbers.
    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let mut parts = id.split(SEP);
        let fields = json!({
            "Service": parts.next(),
            "MonthToDate": parts.next(),
            "Share": parts.next(),
            "Period": parts.next(),
            "Metric": METRIC,
            "Note": "Amounts for the current day are partial and the last few days can still change.",
        });
        Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        })
    }
}

/// One request; any further pages are deliberately left unfetched.
async fn list(client: &Client, start: &str, end: &str) -> Result<Page, AwsError> {
    let out = client
        .get_cost_and_usage()
        .time_period(
            DateInterval::builder()
                .start(start)
                .end(end)
                .build()
                .map_err(build_error)?,
        )
        .granularity(Granularity::Monthly)
        .metrics(METRIC)
        .group_by(
            GroupDefinition::builder()
                .r#type(GroupDefinitionType::Dimension)
                .key("SERVICE")
                .build(),
        )
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: rows(out.results_by_time(), &format!("{start}..{end}")),
        next: None,
    })
}

fn build_error(e: impl std::fmt::Display) -> AwsError {
    AwsError::new(fast_aws_core::ErrorKind::Other, None, e.to_string())
}

fn rows(results: &[ResultByTime], period: &str) -> Vec<Row> {
    let mut by_service: HashMap<String, f64> = HashMap::new();
    let mut unit = String::from("USD");
    let mut estimated = false;
    for result in results {
        estimated |= result.estimated();
        for group in result.groups() {
            let Some(metric) = group.metrics().and_then(|m| m.get(METRIC)) else {
                continue;
            };
            let amount: f64 = metric
                .amount()
                .and_then(|a| a.parse().ok())
                .unwrap_or_default();
            if let Some(u) = metric.unit() {
                unit = u.to_owned();
            }
            let service = group.keys().first().cloned().unwrap_or_default();
            *by_service.entry(service).or_default() += amount;
        }
    }
    let total: f64 = by_service.values().sum();
    let mut services: Vec<(String, f64)> = by_service
        .into_iter()
        .filter(|(_, a)| *a >= 0.005)
        .collect();
    services.sort_by(|a, b| b.1.total_cmp(&a.1));

    let money = |a: f64| {
        if unit == "USD" {
            format!("${a:.2}")
        } else {
            format!("{a:.2} {unit}")
        }
    };
    let total_label = if estimated {
        "TOTAL (estimated)"
    } else {
        "TOTAL"
    };
    let mut rows = vec![Row::new(
        format!("{total_label}{SEP}{}{SEP}100%{SEP}{period}", money(total)),
        vec![
            Cell::toned(total_label, Tone::Good),
            Cell::toned(money(total), Tone::Good),
            "100%".into(),
            "".into(),
        ],
    )];
    rows.extend(services.into_iter().map(|(service, amount)| {
        let share = if total > 0.0 {
            amount / total * 100.0
        } else {
            0.0
        };
        let share_text = format!("{share:.0}%");
        Row::new(
            format!(
                "{service}{SEP}{}{SEP}{share_text}{SEP}{period}",
                money(amount)
            ),
            vec![
                service.into(),
                money(amount).into(),
                share_text.into(),
                bar(share).into(),
            ],
        )
    }));
    rows
}

/// A share as a bar of up to 20 cells, with eighth-block precision.
fn bar(percent: f64) -> String {
    const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (percent.clamp(0.0, 100.0) / 100.0 * 20.0 * 8.0).round() as usize;
    "█".repeat(eighths / 8) + PARTS[eighths % 8]
}

/// The first of the current month to tomorrow (Cost Explorer's end is
/// exclusive), in UTC. On the 1st, the previous month is shown instead, since
/// the new month has no data yet.
fn month_to_date(now_secs: i64) -> (String, String) {
    let today = now_secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(today);
    let (sy, sm) = if d == 1 {
        if m == 1 { (y - 1, 12) } else { (y, m - 1) }
    } else {
        (y, m)
    };
    let (ey, em, ed) = if d == 1 {
        (y, m, 1)
    } else {
        civil_from_days(today + 1)
    };
    (
        format!("{sy:04}-{sm:02}-01"),
        format!("{ey:04}-{em:02}-{ed:02}"),
    )
}

/// Days since 1970-01-01 to a (year, month, day) date, after Howard Hinnant's
/// `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_costexplorer::operation::get_cost_and_usage::GetCostAndUsageOutput;
    use aws_sdk_costexplorer::types::{Group, MetricValue};
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_739), (2026, 10, 13));
        assert_eq!(civil_from_days(20_513), (2026, 3, 1));
        // 2026-10-13 → this month up to tomorrow.
        assert_eq!(
            month_to_date(20_739 * 86_400 + 3_600),
            ("2026-10-01".into(), "2026-10-14".into())
        );
        // 2026-10-31 → the end rolls into November.
        assert_eq!(
            month_to_date(20_757 * 86_400),
            ("2026-10-01".into(), "2026-11-01".into())
        );
        // The 1st shows the whole previous month, across a year boundary too.
        assert_eq!(
            month_to_date(20_727 * 86_400),
            ("2026-09-01".into(), "2026-10-01".into())
        );
        assert_eq!(
            month_to_date(20_454 * 86_400),
            ("2025-12-01".into(), "2026-01-01".into())
        );
    }

    #[test]
    fn bars_scale_to_twenty_cells() {
        assert_eq!(bar(100.0), "█".repeat(20));
        assert_eq!(bar(50.0), "█".repeat(10));
        assert_eq!(bar(0.0), "");
        assert_eq!(bar(2.5), "▌");
    }

    fn group(service: &str, amount: &str) -> Group {
        Group::builder()
            .keys(service)
            .metrics(
                METRIC,
                MetricValue::builder().amount(amount).unit("USD").build(),
            )
            .build()
    }

    #[tokio::test]
    async fn one_request_sorted_by_spend_with_a_total_first() {
        let rule = mock!(Client::get_cost_and_usage)
            .match_requests(|r| {
                r.time_period()
                    .is_some_and(|p| p.start() == "2026-10-01" && p.end() == "2026-10-14")
                    && r.group_by().first().and_then(|g| g.key()) == Some("SERVICE")
            })
            .then_output(|| {
                GetCostAndUsageOutput::builder()
                    .results_by_time(
                        ResultByTime::builder()
                            .groups(group("AWS Lambda", "25"))
                            .groups(group("Amazon EC2", "75"))
                            .groups(group("Tax", "0.001"))
                            .estimated(true)
                            .build(),
                    )
                    .next_page_token("more")
                    .build()
            });
        let client = mock_client!(aws_sdk_costexplorer, [&rule]);
        let page = list(&client, "2026-10-01", "2026-10-14").await.unwrap();

        assert_eq!(page.next, None, "further pages are never fetched");
        let names: Vec<_> = page.rows.iter().map(|r| r.cells[0].text.as_str()).collect();
        assert_eq!(names, ["TOTAL (estimated)", "Amazon EC2", "AWS Lambda"]);
        assert_eq!(page.rows[0].cells[1].text, "$100.00");
        assert_eq!(page.rows[1].cells[2].text, "75%");
        assert_eq!(rule.num_calls(), 1);
    }
}
