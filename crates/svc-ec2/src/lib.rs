//! EC2 resource kinds.

mod instances;

pub use instances::Instances;

/// Registers every EC2 resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Instances);
}
