//! S3 resource kinds: buckets, and the objects inside them browsed by `/` prefix.

mod buckets;
mod objects;

pub use buckets::Buckets;
pub use objects::Objects;

/// Registers every S3 resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Buckets);
    registry.register(Objects);
}

/// The parent ID of an object list: `bucket/prefix`, where the prefix may be empty.
fn parent_id(bucket: &str, prefix: &str) -> String {
    format!("{bucket}/{prefix}")
}

/// Splits a parent ID back into bucket and prefix.
fn split_parent(id: &str) -> (&str, &str) {
    id.split_once('/').unwrap_or((id, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_ids_round_trip() {
        assert_eq!(split_parent(&parent_id("logs", "")), ("logs", ""));
        assert_eq!(
            split_parent(&parent_id("logs", "2026/10/")),
            ("logs", "2026/10/")
        );
        assert_eq!(split_parent("bare"), ("bare", ""));
    }
}
