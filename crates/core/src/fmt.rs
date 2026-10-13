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

/// Timestamps: stored as RFC 3339 UTC text (`2026-10-13T09:00:00Z`) and shown
/// in the display zone, local by default; `Z` in the app switches to UTC.
pub mod time {
    use std::sync::atomic::{AtomicBool, Ordering};

    use jiff::Timestamp;
    use jiff::tz::TimeZone;

    static UTC: AtomicBool = AtomicBool::new(false);

    /// Show times in UTC (`true`) or the system's zone (`false`).
    pub fn set_utc(utc: bool) {
        UTC.store(utc, Ordering::Relaxed);
    }

    pub fn is_utc() -> bool {
        UTC.load(Ordering::Relaxed)
    }

    fn zone() -> TimeZone {
        if is_utc() {
            TimeZone::UTC
        } else {
            TimeZone::system()
        }
    }

    /// The display zone's name for the header, e.g. `UTC` or `IST`.
    pub fn zone_label() -> String {
        if is_utc() {
            return "UTC".into();
        }
        let now = Timestamp::now().to_zoned(TimeZone::system());
        now.strftime("%Z").to_string()
    }

    /// Milliseconds since the epoch as stored text: RFC 3339 in UTC.
    pub fn iso_millis(ms: i64) -> String {
        match Timestamp::from_millisecond(ms) {
            Ok(t) => t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
            Err(_) => ms.to_string(),
        }
    }

    /// A stored timestamp shown in `zone`: `2026-10-13 14:30:00`.
    pub fn show_in(t: Timestamp, zone: &TimeZone) -> String {
        t.to_zoned(zone.clone())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    /// Milliseconds since the epoch, shown in the display zone.
    pub fn show_millis(ms: i64) -> String {
        Timestamp::from_millisecond(ms).map_or_else(|_| ms.to_string(), |t| show_in(t, &zone()))
    }

    /// `text` shown in the display zone if it's a full RFC 3339 timestamp.
    pub fn show_if_timestamp(text: &str) -> Option<String> {
        show_text_in(text, &zone())
    }

    fn show_text_in(text: &str, zone: &TimeZone) -> Option<String> {
        // Only strings that look like one, so names and IDs are never touched.
        if text.len() < 20 || !text.as_bytes().get(10).is_some_and(|b| *b == b'T') {
            return None;
        }
        text.parse::<Timestamp>().ok().map(|t| show_in(t, zone))
    }

    /// A UTC date-time without a zone (`2026-10-13 09:00:00.000`, as Logs
    /// Insights returns them) as milliseconds since the epoch.
    pub fn utc_civil_to_millis(text: &str) -> Option<i64> {
        let civil: jiff::civil::DateTime = text.trim().replace(' ', "T").parse().ok()?;
        civil
            .to_zoned(TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp().as_millisecond())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stored_as_utc_shown_in_a_zone() {
            assert_eq!(iso_millis(0), "1970-01-01T00:00:00Z");
            let t: Timestamp = "2026-10-13T09:00:00Z".parse().unwrap();
            assert_eq!(show_in(t, &TimeZone::UTC), "2026-10-13 09:00:00");
            let plus_two = TimeZone::fixed(jiff::tz::offset(2));
            assert_eq!(show_in(t, &plus_two), "2026-10-13 11:00:00");
        }

        #[test]
        fn only_real_timestamps_are_converted() {
            let plus_two = TimeZone::fixed(jiff::tz::offset(2));
            assert_eq!(
                show_text_in("2026-10-13T09:00:00Z", &plus_two).as_deref(),
                Some("2026-10-13 11:00:00")
            );
            assert_eq!(
                show_text_in("2026-10-13T09:00:00.123Z", &plus_two).as_deref(),
                Some("2026-10-13 11:00:00")
            );
            assert_eq!(show_text_in("i-0123456789abcdefg", &plus_two), None);
            assert_eq!(show_text_in("2026-10-12", &plus_two), None);
        }

        #[test]
        fn insights_times_are_utc() {
            assert_eq!(utc_civil_to_millis("1970-01-01 00:00:01.000"), Some(1000));
            assert_eq!(utc_civil_to_millis("not a time"), None);
        }
    }
}
