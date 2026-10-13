//! Lambda resource kinds: functions and event source mappings.

mod functions;
mod mappings;

pub use functions::Functions;
pub use mappings::EventSourceMappings;

/// Registers every Lambda resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Functions);
    registry.register(EventSourceMappings);
}

/// The last part of an ARN (`...:function:name` → `name`), for compact cells.
fn arn_name(arn: &str) -> &str {
    arn.rsplit([':', '/']).next().unwrap_or(arn)
}

#[cfg(test)]
mod tests {
    #[test]
    fn arn_name_is_the_last_segment() {
        assert_eq!(
            super::arn_name("arn:aws:lambda:eu-west-1:1:function:thumbs"),
            "thumbs"
        );
        assert_eq!(super::arn_name("arn:aws:sqs:eu-west-1:1:jobs"), "jobs");
        assert_eq!(
            super::arn_name("arn:aws:dynamodb:x:1:table/t/stream/2026"),
            "2026"
        );
    }
}
