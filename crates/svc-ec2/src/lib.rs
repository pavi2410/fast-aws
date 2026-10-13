//! EC2 resource kinds.

mod compute;
mod instances;
mod network;

use aws_sdk_ec2::types::Tag;
use serde_json::{Map, Value, json};

pub use compute::{Addresses, Images, KeyPairs, Volumes};
pub use instances::Instances;
pub use network::{SecurityGroups, Subnets, Vpcs};

/// Registers every EC2 resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Instances);
    registry.register(SecurityGroups);
    registry.register(Vpcs);
    registry.register(Subnets);
    registry.register(Volumes);
    registry.register(Images);
    registry.register(KeyPairs);
    registry.register(Addresses);
}

/// The `Name` tag, which is what the console shows as a resource's name.
fn name(tags: &[Tag]) -> Option<&str> {
    tags.iter()
        .find(|t| t.key() == Some("Name"))
        .and_then(|t| t.value())
}

/// Tags as a JSON object, for describe views.
fn tags(tags: &[Tag]) -> Value {
    let map: Map<String, Value> = tags
        .iter()
        .map(|t| (t.key().unwrap_or_default().to_owned(), json!(t.value())))
        .collect();
    Value::Object(map)
}
