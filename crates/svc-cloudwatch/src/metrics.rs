//! `cloudwatch:metric`: the metrics published for one resource, found with
//! `ListMetrics` (free) so metrics the CloudWatch agent adds (`CWAgent`:
//! memory, disk) show up next to the built-in ones.
//!
//! A resource's metrics list has the parent ID `DimensionName=value`
//! (`InstanceId=i-0abc`).
//!
//! Each row carries the metric's full dimension set. `GetMetricData` only
//! matches a metric whose dimensions are all given, so a guessed set (just
//! `InstanceId` for an agent metric published with `ImageId` and `path` too)
//! would come back empty.

use async_trait::async_trait;
use aws_sdk_cloudwatch::Client;
use aws_sdk_cloudwatch::types::{DimensionFilter, Metric};
use fast_aws_core::metrics::{MetricSpec, Summary};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

/// Separates the parts of a row ID.
const SEP: char = '\u{1f}';
const AGENT: &str = "CWAgent";

const COLUMNS: &[Column] = &[
    Column::fixed("NAMESPACE", 12),
    Column::fill("METRIC", 3),
    Column::fill("DIMENSIONS", 3),
    Column::spark("24h", 20, "6h"),
];

pub struct Metrics;

#[async_trait]
impl ResourceKind for Metrics {
    kind_info! {
        id: "cloudwatch:metric",
        title: "Metrics",
        aliases: ["metrics"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    fn sparklines_on_open(&self) -> bool {
        true
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        describe(id)
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let (name, value) = query
        .parent
        .as_ref()
        .and_then(|p| p.id.split_once('='))
        .ok_or_else(|| AwsError::not_found("metrics need a resource"))?;
    let out = client
        .list_metrics()
        .dimensions(DimensionFilter::builder().name(name).value(value).build())
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let mut metrics: Vec<&Metric> = out.metrics().iter().collect();
    metrics.sort_by_key(|m| (rank(m.namespace()), m.namespace(), m.metric_name(), dims(m)));
    let mut rows: Vec<Row> = metrics.iter().map(|m| row(m, name)).collect();
    // On a single page, say why memory and disk are missing.
    let single_page = query.next.is_none() && out.next_token().is_none();
    let has_agent = metrics.iter().any(|m| m.namespace() == Some(AGENT));
    if single_page && !rows.is_empty() && !has_agent {
        rows.push(Row::new(
            "hint",
            vec![
                Cell::toned(AGENT, Tone::Dim),
                Cell::toned(
                    "no memory or disk metrics: they need the CloudWatch agent",
                    Tone::Dim,
                ),
                "".into(),
                "".into(),
            ],
        ));
    }
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

/// The built-in namespace first, then the agent, then anything custom.
fn rank(namespace: Option<&str>) -> u8 {
    match namespace {
        Some(n) if n.starts_with("AWS/") => 0,
        Some(AGENT) => 1,
        _ => 2,
    }
}

/// `(name, value)` pairs, in a stable order.
fn dims(m: &Metric) -> Vec<(String, String)> {
    let mut d: Vec<_> = m
        .dimensions()
        .iter()
        .map(|d| {
            (
                d.name().unwrap_or_default().to_owned(),
                d.value().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    d.sort();
    d
}

fn row(m: &Metric, filter: &str) -> Row {
    let namespace = m.namespace().unwrap_or_default();
    let name = m.metric_name().unwrap_or_default();
    let dimensions = dims(m);
    // The parent's own dimension is the same on every row; show the rest.
    let rest: Vec<String> = dimensions
        .iter()
        .filter(|(n, _)| n != filter)
        .map(|(n, v)| format!("{n}={v}"))
        .collect();
    let id = format!(
        "{namespace}{SEP}{name}{SEP}{}",
        dimensions
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join(&SEP.to_string())
    );
    let (stat, summary) = stat_for(name);
    Row::new(
        id,
        vec![
            namespace.into(),
            name.into(),
            rest.join(" ").into(),
            "".into(),
        ],
    )
    .metric(MetricSpec {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        dimensions,
        stat,
        summary,
        unit: unit_for(name),
        column: 3,
    })
}

/// How to read a metric. `ListMetrics` gives no unit or statistic, so this goes
/// by name: counts and bytes are summed, flags take the maximum, the rest
/// (utilisation and the like) are averaged and shown by their peak.
fn stat_for(name: &str) -> (&'static str, Summary) {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("statuscheckfailed") {
        ("Maximum", Summary::Peak)
    } else if ["network", "bytes", "ops", "packets", "count"]
        .iter()
        .any(|w| lower.contains(w))
    {
        ("Sum", Summary::Total)
    } else {
        ("Average", Summary::Peak)
    }
}

fn unit_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.contains("percent") || lower.contains("utilization") || lower.contains("usage_") {
        "%"
    } else {
        ""
    }
}

fn describe(id: &str) -> Result<Detail, AwsError> {
    if id == "hint" {
        return Err(AwsError::not_found("the CloudWatch agent note"));
    }
    let mut parts = id.split(SEP);
    let (Some(namespace), Some(name)) = (parts.next(), parts.next()) else {
        return Err(AwsError::not_found(id));
    };
    let dimensions: serde_json::Map<String, serde_json::Value> = parts
        .filter_map(|d| d.split_once('='))
        .map(|(n, v)| (n.to_owned(), json!(v)))
        .collect();
    let (stat, _) = stat_for(name);
    let fields = json!({
        "Namespace": namespace,
        "MetricName": name,
        "Dimensions": dimensions,
        "StatisticUsed": stat,
    });
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatch::operation::list_metrics::ListMetricsOutput;
    use aws_sdk_cloudwatch::types::Dimension;
    use aws_smithy_mocks::{mock, mock_client};

    fn metric(namespace: &str, name: &str, dims: &[(&str, &str)]) -> Metric {
        Metric::builder()
            .namespace(namespace)
            .metric_name(name)
            .set_dimensions(Some(
                dims.iter()
                    .map(|(n, v)| Dimension::builder().name(*n).value(*v).build())
                    .collect(),
            ))
            .build()
    }

    fn query() -> ListQuery {
        ListQuery {
            parent: Some(Parent {
                id: "InstanceId=i-1".into(),
                label: "web-1 metrics".into(),
                region: None,
            }),
            next: None,
        }
    }

    #[tokio::test]
    async fn agent_metrics_keep_every_dimension_they_were_published_with() {
        let rule = mock!(Client::list_metrics)
            .match_requests(|r| {
                r.namespace().is_none()
                    && r.dimensions().len() == 1
                    && r.dimensions()[0].name() == Some("InstanceId")
                    && r.dimensions()[0].value() == Some("i-1")
            })
            .then_output(|| {
                ListMetricsOutput::builder()
                    .metrics(metric(
                        "CWAgent",
                        "mem_used_percent",
                        &[
                            ("InstanceId", "i-1"),
                            ("ImageId", "ami-9"),
                            ("InstanceType", "t3.small"),
                        ],
                    ))
                    .metrics(metric(
                        "AWS/EC2",
                        "CPUUtilization",
                        &[("InstanceId", "i-1")],
                    ))
                    .metrics(metric("AWS/EC2", "NetworkIn", &[("InstanceId", "i-1")]))
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudwatch, [&rule]);
        let page = list(&client, &query()).await.unwrap();

        // Built-in first, then the agent; no hint, since the agent is there.
        let names: Vec<_> = page.rows.iter().map(|r| r.cells[1].text.as_str()).collect();
        assert_eq!(names, ["CPUUtilization", "NetworkIn", "mem_used_percent"]);

        let mem = &page.rows[2];
        let spec = &mem.metrics[0];
        assert_eq!(spec.namespace, "CWAgent");
        assert_eq!(
            spec.dimensions,
            [
                ("ImageId".to_owned(), "ami-9".to_owned()),
                ("InstanceId".to_owned(), "i-1".to_owned()),
                ("InstanceType".to_owned(), "t3.small".to_owned()),
            ]
        );
        assert_eq!((spec.stat, spec.unit, spec.column), ("Average", "%", 3));
        // The shared InstanceId isn't repeated in the cell.
        assert_eq!(mem.cells[2].text, "ImageId=ami-9 InstanceType=t3.small");
        assert_eq!(page.rows[1].metrics[0].stat, "Sum");
    }

    #[tokio::test]
    async fn a_missing_agent_is_explained_once() {
        let rule = mock!(Client::list_metrics).then_output(|| {
            ListMetricsOutput::builder()
                .metrics(metric(
                    "AWS/EC2",
                    "CPUUtilization",
                    &[("InstanceId", "i-1")],
                ))
                .build()
        });
        let client = mock_client!(aws_sdk_cloudwatch, [&rule]);
        let page = list(&client, &query()).await.unwrap();
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[1].id, "hint");
        assert!(page.rows[1].cells[1].text.contains("CloudWatch agent"));
        assert!(page.rows[1].metrics.is_empty());
    }

    #[tokio::test]
    async fn no_hint_on_a_page_that_is_not_the_only_one() {
        let rule = mock!(Client::list_metrics).then_output(|| {
            ListMetricsOutput::builder()
                .metrics(metric(
                    "AWS/EC2",
                    "CPUUtilization",
                    &[("InstanceId", "i-1")],
                ))
                .next_token("more")
                .build()
        });
        let client = mock_client!(aws_sdk_cloudwatch, [&rule]);
        let page = list(&client, &query()).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.next.as_deref(), Some("more"));
    }

    #[test]
    fn describe_reads_the_row_id() {
        let id = format!("CWAgent{SEP}disk_used_percent{SEP}InstanceId=i-1{SEP}path=/");
        let detail = describe(&id).unwrap();
        assert_eq!(detail.fields["Dimensions"]["path"], "/");
        assert!(describe("hint").is_err());
    }
}
