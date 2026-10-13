//! EC2 resource kinds.

mod instances;

pub use instances::Instances;

/// Registers every EC2 resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Instances);
}

/// Formats how long ago something happened, in the largest whole unit (`41d`, `3h`).
fn age(now_secs: i64, then_secs: i64) -> String {
    let secs = (now_secs - then_secs).max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 365 * 86_400 => format!("{}d", s / 86_400),
        s => format!("{}y", s / (365 * 86_400)),
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::age;

    #[test]
    fn age_uses_the_largest_whole_unit() {
        assert_eq!(age(100, 88), "12s");
        assert_eq!(age(10_000, 10_000 - 125), "2m");
        assert_eq!(age(100_000, 100_000 - 3 * 3_600), "3h");
        assert_eq!(age(10_000_000, 10_000_000 - 41 * 86_400), "41d");
        assert_eq!(age(100_000_000, 100_000_000 - 800 * 86_400), "2y");
        assert_eq!(age(0, 50), "0s");
    }
}
