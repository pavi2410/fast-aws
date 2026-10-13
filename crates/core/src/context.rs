//! The AWS context a view runs in (profile + region), and the factory that builds
//! SDK configs for it.

use std::collections::HashMap;

use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_smithy_types::retry::RetryConfig;
use tokio::sync::Mutex;

use crate::error::AwsError;

/// Used when neither the profile nor the environment names a region.
pub const FALLBACK_REGION: &str = "us-east-1";

/// Everything a service call needs: which profile and region, and the SDK config
/// (credentials, retry policy) for them. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub profile: String,
    pub region: String,
    config: SdkConfig,
}

impl Ctx {
    /// Wraps an existing config. Used by tests and by the factory.
    pub fn new(profile: impl Into<String>, config: SdkConfig) -> Self {
        let region = config
            .region()
            .map(|r| r.as_ref().to_owned())
            .unwrap_or_else(|| FALLBACK_REGION.to_owned());
        Self {
            profile: profile.into(),
            region,
            config,
        }
    }

    pub fn config(&self) -> &SdkConfig {
        &self.config
    }

    /// The same credentials in another region, for global or pinned services
    /// (IAM, CloudFront, Cost Explorer) and per-bucket S3 calls.
    pub fn in_region(&self, region: &str) -> Self {
        let config = self
            .config
            .to_builder()
            .region(Region::new(region.to_owned()))
            .build();
        Self {
            profile: self.profile.clone(),
            region: region.to_owned(),
            config,
        }
    }
}

/// Builds and caches one SDK config per profile. Other regions reuse that config's
/// credentials provider, so switching region never triggers a new SSO or STS call.
#[derive(Default)]
pub struct ConfigFactory {
    by_profile: Mutex<HashMap<String, Ctx>>,
}

impl ConfigFactory {
    /// Returns a context for `profile` in `region`.
    ///
    /// The region is always given, so the SDK never runs its own region lookup,
    /// which could otherwise probe instance metadata and stall on a laptop.
    pub async fn ctx(&self, profile: &str, region: &str) -> Ctx {
        let mut cache = self.by_profile.lock().await;
        let base = match cache.get(profile) {
            Some(ctx) => ctx.clone(),
            None => {
                let config = aws_config::defaults(BehaviorVersion::latest())
                    .profile_name(profile)
                    .region(Region::new(region.to_owned()))
                    .retry_config(RetryConfig::adaptive())
                    .load()
                    .await;
                let ctx = Ctx::new(profile, config);
                cache.insert(profile.to_owned(), ctx.clone());
                ctx
            }
        };
        if base.region == region {
            base
        } else {
            base.in_region(region)
        }
    }
}

/// The region to start in when none was asked for: `AWS_REGION`, then
/// `AWS_DEFAULT_REGION`, then the profile's `region`, then [`FALLBACK_REGION`].
/// This is the SDK's order, minus the instance-metadata lookup.
pub fn default_region(profile_region: Option<&str>) -> String {
    default_region_from(|k| std::env::var(k).ok(), profile_region)
}

fn default_region_from(
    env: impl Fn(&str) -> Option<String>,
    profile_region: Option<&str>,
) -> String {
    ["AWS_REGION", "AWS_DEFAULT_REGION"]
        .into_iter()
        .find_map(|k| env(k).filter(|v| !v.is_empty()))
        .or_else(|| profile_region.map(str::to_owned))
        .unwrap_or_else(|| FALLBACK_REGION.to_owned())
}

/// Who the current credentials belong to, shown in the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    pub account: String,
    pub arn: String,
}

/// Calls `sts:GetCallerIdentity`. It needs no IAM permission, so a failure here means
/// the credentials themselves are the problem (expired SSO, missing keys, network).
pub async fn caller_identity(ctx: &Ctx) -> Result<CallerIdentity, AwsError> {
    let out = aws_sdk_sts::Client::new(ctx.config())
        .get_caller_identity()
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(CallerIdentity {
        account: out.account.unwrap_or_default(),
        arn: out.arn.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_region_keeps_profile_and_credentials() {
        let config = SdkConfig::builder()
            .region(Region::new("eu-west-1"))
            .behavior_version(BehaviorVersion::latest())
            .build();
        let ctx = Ctx::new("acme-dev", config);
        assert_eq!(ctx.region, "eu-west-1");

        let other = ctx.in_region("us-east-1");
        assert_eq!(other.profile, "acme-dev");
        assert_eq!(other.region, "us-east-1");
        assert_eq!(other.config().region().unwrap().as_ref(), "us-east-1");
    }

    #[test]
    fn default_region_follows_the_sdk_order_without_imds() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(default_region_from(env(&[]), None), FALLBACK_REGION);
        assert_eq!(
            default_region_from(env(&[]), Some("eu-west-1")),
            "eu-west-1"
        );
        assert_eq!(
            default_region_from(
                env(&[("AWS_DEFAULT_REGION", "ap-south-1")]),
                Some("eu-west-1")
            ),
            "ap-south-1"
        );
        assert_eq!(
            default_region_from(
                env(&[
                    ("AWS_REGION", "us-west-2"),
                    ("AWS_DEFAULT_REGION", "ap-south-1")
                ]),
                Some("eu-west-1")
            ),
            "us-west-2"
        );
        assert_eq!(
            default_region_from(env(&[("AWS_REGION", "")]), Some("eu-west-1")),
            "eu-west-1"
        );
    }

    #[test]
    fn missing_region_falls_back() {
        let ctx = Ctx::new("p", SdkConfig::builder().build());
        assert_eq!(ctx.region, FALLBACK_REGION);
    }
}
