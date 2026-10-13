//! Small formatting helpers shared by the service crates, so cells look the same
//! everywhere.

/// Seconds since the Unix epoch, now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// How long ago something happened, in the largest whole unit (`41d`, `3h`).
pub fn age(now_secs: i64, then_secs: i64) -> String {
    let secs = (now_secs - then_secs).max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 365 * 86_400 => format!("{}d", s / 86_400),
        s => format!("{}y", s / (365 * 86_400)),
    }
}

/// [`age`] of an optional timestamp in seconds, or `-`.
pub fn age_of(then_secs: Option<i64>) -> String {
    then_secs
        .map(|t| age(now_secs(), t))
        .unwrap_or_else(|| "-".into())
}

/// A Unix timestamp in milliseconds as UTC, `2026-10-13 09:00:00`.
pub fn utc_millis(ms: i64) -> String {
    use aws_smithy_types::date_time::{DateTime, Format};
    DateTime::from_millis(ms)
        .fmt(Format::DateTime)
        .map(|s| {
            s.replace('T', " ")
                .trim_end_matches('Z')
                .chars()
                .take(19)
                .collect()
        })
        .unwrap_or_else(|_| ms.to_string())
}

/// The date part (`2026-10-13`) of an ISO 8601 timestamp string.
pub fn date_part(iso: Option<&str>) -> String {
    or_dash(iso.map(|s| s.get(..10).unwrap_or(s)))
}

/// The text, or `-` when there is none.
pub fn or_dash(text: Option<&str>) -> String {
    match text {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => "-".to_owned(),
    }
}

/// A byte count in binary units (`512 B`, `1.5 KiB`, `3.2 GiB`).
pub fn bytes(n: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = n.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Implements the descriptive methods of [`ResourceKind`](crate::ResourceKind)
/// inside an `impl` block, leaving `list` and `describe` to be written out.
///
/// ```ignore
/// impl ResourceKind for Vpcs {
///     kind_info! {
///         id: "ec2:vpc", title: "VPCs", aliases: ["vpc", "vpcs"],
///         scope: Scope::Regional, columns: COLUMNS,
///     }
///     async fn list(..) { .. }
///     async fn describe(..) { .. }
/// }
/// ```
#[macro_export]
macro_rules! kind_info {
    (
        id: $id:expr,
        title: $title:expr,
        aliases: [$($alias:expr),* $(,)?],
        scope: $scope:expr,
        columns: $columns:expr
        $(, palette: $palette:expr)?
        $(,)?
    ) => {
        fn id(&self) -> &'static str {
            $id
        }
        fn title(&self) -> &'static str {
            $title
        }
        fn aliases(&self) -> &'static [&'static str] {
            &[$($alias),*]
        }
        fn scope(&self) -> $crate::Scope {
            $scope
        }
        fn columns(&self) -> &'static [$crate::Column] {
            $columns
        }
        $(
            fn in_palette(&self) -> bool {
                $palette
            }
        )?
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_uses_the_largest_whole_unit() {
        assert_eq!(age(100, 88), "12s");
        assert_eq!(age(10_000, 10_000 - 125), "2m");
        assert_eq!(age(100_000, 100_000 - 3 * 3_600), "3h");
        assert_eq!(age(10_000_000, 10_000_000 - 41 * 86_400), "41d");
        assert_eq!(age(100_000_000, 100_000_000 - 800 * 86_400), "2y");
        assert_eq!(age(0, 50), "0s");
        assert_eq!(age_of(None), "-");
    }

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(bytes(-1), "-1 B");
    }

    #[test]
    fn utc_timestamps() {
        assert_eq!(utc_millis(0), "1970-01-01 00:00:00");
        assert_eq!(utc_millis(1_760_346_000_123), "2025-10-13 09:00:00");
        assert_eq!(
            date_part(Some("2026-10-12T09:13:44.000+0000")),
            "2026-10-12"
        );
        assert_eq!(date_part(None), "-");
    }

    #[test]
    fn dash_for_missing_text() {
        assert_eq!(or_dash(None), "-");
        assert_eq!(or_dash(Some("")), "-");
        assert_eq!(or_dash(Some("x")), "x");
    }
}
