//! [`MetricSource`] for sparklines: one `GetMetricData` call for up to 500
//! metrics (billed per metric requested), hourly points.

use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_cloudwatch::Client;
use aws_sdk_cloudwatch::primitives::DateTime;
use aws_sdk_cloudwatch::types::{Dimension, Metric, MetricDataQuery, MetricStat};
use fast_aws_core::metrics::{MetricSource, MetricSpec};
use fast_aws_core::{AwsError, Ctx};

/// The most metrics one `GetMetricData` call accepts.
const MAX_QUERIES: usize = 500;
const HOUR: i64 = 3_600;

pub struct CloudWatchMetrics;

#[async_trait]
impl MetricSource for CloudWatchMetrics {
    async fn fetch(
        &self,
        ctx: &Ctx,
        metrics: &[(String, MetricSpec)],
        hours: u32,
    ) -> Result<HashMap<(String, usize), Vec<f64>>, AwsError> {
        let now = fast_aws_core::fmt::now_secs();
        fetch(&Client::new(ctx.config()), metrics, hours, now).await
    }
}

async fn fetch(
    client: &Client,
    metrics: &[(String, MetricSpec)],
    hours: u32,
    now: i64,
) -> Result<HashMap<(String, usize), Vec<f64>>, AwsError> {
    // Whole hours, so every row's points line up.
    let end = (now / HOUR + 1) * HOUR;
    let start = end - i64::from(hours) * HOUR;
    let mut points = HashMap::new();
    for chunk in metrics.chunks(MAX_QUERIES) {
        let queries: Vec<MetricDataQuery> = chunk
            .iter()
            .enumerate()
            .map(|(i, (_, spec))| query(i, spec))
            .collect();
        let mut next = None;
        loop {
            let out = client
                .get_metric_data()
                .set_metric_data_queries(Some(queries.clone()))
                .start_time(DateTime::from_secs(start))
                .end_time(DateTime::from_secs(end))
                .set_next_token(next)
                .send()
                .await
                .map_err(AwsError::from_sdk)?;
            for result in out.metric_data_results() {
                // Query IDs are `m<index into chunk>`.
                let Some(i) = result
                    .id()
                    .and_then(|id| id.strip_prefix('m')?.parse::<usize>().ok())
                else {
                    continue;
                };
                let Some((row, spec)) = chunk.get(i) else {
                    continue;
                };
                let series = points
                    .entry((row.clone(), spec.column))
                    .or_insert_with(|| vec![0.0; hours as usize]);
                for (t, v) in result.timestamps().iter().zip(result.values()) {
                    let slot = (t.secs() - start) / HOUR;
                    if let Some(p) = usize::try_from(slot).ok().and_then(|s| series.get_mut(s)) {
                        *p = *v;
                    }
                }
            }
            next = out.next_token;
            if next.is_none() {
                break;
            }
        }
        // Rows with no data at all still get a (flat) line.
        for (row, spec) in chunk {
            points
                .entry((row.clone(), spec.column))
                .or_insert_with(|| vec![0.0; hours as usize]);
        }
    }
    Ok(points)
}

fn query(i: usize, spec: &MetricSpec) -> MetricDataQuery {
    let dimensions = spec
        .dimensions
        .iter()
        .map(|(name, value)| Dimension::builder().name(*name).value(value).build())
        .collect();
    let metric = Metric::builder()
        .namespace(spec.namespace)
        .metric_name(spec.name)
        .set_dimensions(Some(dimensions))
        .build();
    MetricDataQuery::builder()
        .id(format!("m{i}"))
        .metric_stat(
            MetricStat::builder()
                .metric(metric)
                .period(HOUR as i32)
                .stat(spec.stat)
                .build(),
        )
        .return_data(true)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudwatch::operation::get_metric_data::GetMetricDataOutput;
    use aws_sdk_cloudwatch::types::MetricDataResult;
    use aws_smithy_mocks::{mock, mock_client};
    use fast_aws_core::metrics::Summary;

    fn spec(function: &str) -> MetricSpec {
        MetricSpec {
            namespace: "AWS/Lambda",
            name: "Errors",
            dimensions: vec![("FunctionName", function.to_owned())],
            stat: "Sum",
            summary: Summary::Total,
            unit: "",
            column: 6,
        }
    }

    #[tokio::test]
    async fn one_call_fills_hourly_series_per_row() {
        let now = 10 * HOUR + 120; // 10:02
        let rule = mock!(Client::get_metric_data)
            .match_requests(|r| {
                r.metric_data_queries().len() == 2
                    && r.start_time() == Some(&DateTime::from_secs(7 * HOUR))
                    && r.end_time() == Some(&DateTime::from_secs(11 * HOUR))
            })
            .then_output(|| {
                GetMetricDataOutput::builder()
                    .metric_data_results(
                        MetricDataResult::builder()
                            .id("m0")
                            .timestamps(DateTime::from_secs(8 * HOUR))
                            .values(3.0)
                            .timestamps(DateTime::from_secs(10 * HOUR))
                            .values(1.0)
                            .build(),
                    )
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudwatch, [&rule]);
        let metrics = vec![
            ("thumbs".to_owned(), spec("thumbs")),
            ("idle".to_owned(), spec("idle")),
        ];
        let points = fetch(&client, &metrics, 4, now).await.unwrap();
        assert_eq!(points[&("thumbs".to_owned(), 6)], [0.0, 3.0, 0.0, 1.0]);
        assert_eq!(points[&("idle".to_owned(), 6)], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn a_rows_metrics_stay_apart() {
        let rule = mock!(Client::get_metric_data).then_output(|| {
            GetMetricDataOutput::builder()
                .metric_data_results(
                    MetricDataResult::builder()
                        .id("m0")
                        .timestamps(DateTime::from_secs(0))
                        .values(3.0)
                        .build(),
                )
                .metric_data_results(
                    MetricDataResult::builder()
                        .id("m1")
                        .timestamps(DateTime::from_secs(HOUR))
                        .values(7.0)
                        .build(),
                )
                .build()
        });
        let client = mock_client!(aws_sdk_cloudwatch, [&rule]);
        let other = MetricSpec {
            column: 7,
            ..spec("thumbs")
        };
        let metrics = vec![
            ("thumbs".to_owned(), spec("thumbs")),
            ("thumbs".to_owned(), other),
        ];
        let points = fetch(&client, &metrics, 2, HOUR).await.unwrap();
        assert_eq!(points[&("thumbs".to_owned(), 6)], [3.0, 0.0]);
        assert_eq!(points[&("thumbs".to_owned(), 7)], [0.0, 7.0]);
    }
}
