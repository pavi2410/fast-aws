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

/// How sparklines are drawn (`sparkline = "dots"` or `"bars"` in the config).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SparkStyle {
    /// Braille dots, two points per character.
    #[default]
    Dots,
    /// Block bars, one per two points (merged), so both styles are as wide.
    Bars,
}

/// A sparkline in `style`, then the summary.
pub fn sparkline_in(style: SparkStyle, points: &[f64], summary: Summary, unit: &str) -> String {
    match style {
        SparkStyle::Dots => sparkline(points, summary, unit),
        SparkStyle::Bars => bars(points, summary, unit),
    }
}

/// `▁▃█▅ 14`: block bars, each merging two points (summed for totals, the
/// higher for peaks), scaled to the line's maximum.
fn bars(points: &[f64], summary: Summary, unit: &str) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let merged: Vec<f64> = points
        .chunks(2)
        .map(|pair| match summary {
            Summary::Total => pair.iter().sum(),
            Summary::Peak => pair.iter().copied().fold(0.0_f64, f64::max),
        })
        .collect();
    let max = merged.iter().copied().fold(0.0_f64, f64::max);
    let line: String = merged
        .iter()
        .map(|p| {
            if max <= 0.0 {
                BARS[0]
            } else {
                BARS[((p / max) * 7.0).round().clamp(0.0, 7.0) as usize]
            }
        })
        .collect();
    format!("{line} {}{unit}", summary_value(points, summary))
}

fn summary_value(points: &[f64], summary: Summary) -> String {
    let value = match summary {
        Summary::Total => points.iter().sum::<f64>(),
        Summary::Peak => points.iter().copied().fold(0.0_f64, f64::max),
    };
    if value.fract() == 0.0 || value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// A dotted sparkline then the summary, e.g. `⣀⣠⣴⣾ 14`: braille dots, two
/// points per character, each one to four dots high (one dot for zero),
/// scaled to the line's own maximum.
pub fn sparkline(points: &[f64], summary: Summary, unit: &str) -> String {
    // Braille dot bits, bottom to top, for the left and right columns.
    const LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
    const RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
    let max = points.iter().copied().fold(0.0_f64, f64::max);
    let height = |p: f64| -> usize {
        if max <= 0.0 || p <= 0.0 {
            1
        } else {
            1 + ((p / max) * 3.0).round().clamp(0.0, 3.0) as usize
        }
    };
    let column = |bits: &[u32; 4], p: f64| bits[..height(p)].iter().sum::<u32>();
    let dots: String = points
        .chunks(2)
        .map(|pair| {
            let mut cell = column(&LEFT, pair[0]);
            if let Some(&right) = pair.get(1) {
                cell += column(&RIGHT, right);
            }
            char::from_u32(0x2800 + cell).unwrap_or(' ')
        })
        .collect();
    format!("{dots} {}{unit}", summary_value(points, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparklines_are_dots_scaled_to_their_maximum() {
        // Zero is one dot, the maximum four; two points per character.
        assert_eq!(sparkline(&[0.0, 0.0], Summary::Total, ""), "⣀ 0");
        assert_eq!(sparkline(&[0.0, 4.0], Summary::Total, ""), "⣸ 4");
        assert_eq!(sparkline(&[4.0, 4.0, 0.0], Summary::Total, ""), "⣿⡀ 8");
        assert_eq!(sparkline(&[10.0, 42.5], Summary::Peak, "%"), "⣼ 42.5%");
    }

    #[test]
    fn bars_merge_pairs_so_both_styles_are_as_wide() {
        let points = [0.0, 1.0, 2.0, 2.0, 0.0, 0.0];
        assert_eq!(
            sparkline_in(SparkStyle::Bars, &points, Summary::Total, ""),
            "▃█▁ 5"
        );
        assert_eq!(
            sparkline_in(SparkStyle::Dots, &points, Summary::Total, "")
                .chars()
                .count(),
            5
        );
    }
}
