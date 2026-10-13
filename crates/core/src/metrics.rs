//! Sparklines: a row can name one CloudWatch metric, fetched for the rows near
//! the cursor in one batched call and drawn into one of its cells.
//!
//! Fetching is billed per metric requested (`GetMetricData`), so only a window
//! of rows is fetched, once per list, and it can be turned off.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::context::Ctx;
use crate::error::AwsError;

/// How a metric's points become the number printed after the sparkline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Summary {
    /// The sum over the window (counts such as errors).
    Total,
    /// The highest point (utilisation).
    Peak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricSpec {
    pub namespace: &'static str,
    pub name: &'static str,
    pub dimensions: Vec<(&'static str, String)>,
    /// `Sum`, `Average`, `Maximum`, …
    pub stat: &'static str,
    pub summary: Summary,
    /// Printed after the number, e.g. `%`.
    pub unit: &'static str,
    /// The row cell the sparkline is drawn into.
    pub column: usize,
}

/// Something that can fetch metric points: the CloudWatch service crate.
#[async_trait]
pub trait MetricSource: Send + Sync {
    /// Hourly points over the last `hours`, oldest first, for each row ID.
    /// Missing hours are 0.
    async fn fetch(
        &self,
        ctx: &Ctx,
        metrics: &[(String, MetricSpec)],
        hours: u32,
    ) -> Result<HashMap<String, Vec<f64>>, AwsError>;
}

/// `▁▂▅█▃ 14` or `▁▁▁▁ 0%`: the points scaled to their own maximum, then the
/// summary.
pub fn sparkline(points: &[f64], summary: Summary, unit: &str) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = points.iter().copied().fold(0.0_f64, f64::max);
    let bars: String = points
        .iter()
        .map(|p| {
            if max <= 0.0 {
                BARS[0]
            } else {
                BARS[((p / max) * 7.0).round().clamp(0.0, 7.0) as usize]
            }
        })
        .collect();
    let value = match summary {
        Summary::Total => points.iter().sum::<f64>(),
        Summary::Peak => max,
    };
    let value = if value.fract() == 0.0 || value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    format!("{bars} {value}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparklines_scale_to_their_maximum() {
        assert_eq!(
            sparkline(&[0.0, 1.0, 2.0, 4.0], Summary::Total, ""),
            "▁▃▅█ 7"
        );
        assert_eq!(sparkline(&[0.0, 0.0], Summary::Total, ""), "▁▁ 0");
        assert_eq!(sparkline(&[10.0, 42.5], Summary::Peak, "%"), "▃█ 42.5%");
    }
}
