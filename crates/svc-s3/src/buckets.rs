//! `s3:bucket`: S3 buckets.

use async_trait::async_trait;
use aws_sdk_s3::Client;
use aws_sdk_s3::types::Bucket;
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, kind_info,
};
use serde_json::{Value, json};

pub struct Buckets;

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("REGION", 14),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Buckets {
    kind_info! {
        id: "s3:bucket",
        title: "S3 Buckets",
        aliases: ["s3", "buckets", "bucket"],
        scope: Scope::Regional,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    /// Runs in the bucket's own region (the row carries it).
    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        describe(&Client::new(ctx.config()), id).await
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    // `BucketRegion` is only returned when the request has a parameter, so always
    // send `MaxBuckets`.
    let out = client
        .list_buckets()
        .max_buckets(1000)
        .set_continuation_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.buckets().iter().map(row).collect(),
        next: out.continuation_token,
    })
}

fn row(b: &Bucket) -> Row {
    let name = b.name().unwrap_or_default();
    let region = b.bucket_region().map(str::to_owned);
    let parent = Parent {
        id: crate::parent_id(name, ""),
        label: name.to_owned(),
        region: region.clone(),
    };
    Row::new(
        name,
        vec![
            name.into(),
            or_dash(b.bucket_region()).into(),
            age_of(b.creation_date().map(|t| t.secs())).into(),
        ],
    )
    .drill("s3:object", parent)
    .in_region(region)
}

/// Error codes S3 uses for "this setting isn't configured".
const NOT_CONFIGURED: &[&str] = &[
    "NoSuchTagSet",
    "NoSuchPublicAccessBlockConfiguration",
    "ServerSideEncryptionConfigurationNotFoundError",
    "NoSuchBucketPolicy",
];

/// Each setting is its own call; one being denied or unset doesn't hide the rest.
async fn describe(client: &Client, bucket: &str) -> Result<Detail, AwsError> {
    let (versioning, encryption, public_access, policy_status, tagging) = tokio::join!(
        client.get_bucket_versioning().bucket(bucket).send(),
        client.get_bucket_encryption().bucket(bucket).send(),
        client.get_public_access_block().bucket(bucket).send(),
        client.get_bucket_policy_status().bucket(bucket).send(),
        client.get_bucket_tagging().bucket(bucket).send(),
    );
    // Versioning needs only s3:GetBucketVersioning and always answers for an
    // existing bucket, so its failure is the bucket's failure.
    let versioning = versioning.map_err(AwsError::from_sdk)?;
    let or_error = |r: Result<Value, AwsError>, none: Value| match r {
        Ok(v) => v,
        // S3 reports a setting that was never configured as an error; that
        // just means there is none.
        Err(e)
            if e.code
                .as_deref()
                .is_some_and(|c| NOT_CONFIGURED.contains(&c)) =>
        {
            none
        }
        Err(e) => json!(format!("⚠ {e}")),
    };

    let encryption = encryption.map_err(AwsError::from_sdk).map(|o| {
        let rules = o
            .server_side_encryption_configuration()
            .map(|c| c.rules())
            .unwrap_or_default();
        json!(
            rules
                .iter()
                .filter_map(|r| r.apply_server_side_encryption_by_default())
                .map(|d| json!({
                    "Algorithm": d.sse_algorithm().as_str(),
                    "KmsKeyId": d.kms_master_key_id(),
                }))
                .collect::<Vec<_>>()
        )
    });
    let public_access = public_access.map_err(AwsError::from_sdk).map(|o| {
        let c = o.public_access_block_configuration();
        json!({
            "BlockPublicAcls": c.and_then(|c| c.block_public_acls()),
            "IgnorePublicAcls": c.and_then(|c| c.ignore_public_acls()),
            "BlockPublicPolicy": c.and_then(|c| c.block_public_policy()),
            "RestrictPublicBuckets": c.and_then(|c| c.restrict_public_buckets()),
        })
    });
    let is_public = policy_status
        .map_err(AwsError::from_sdk)
        .map(|o| json!(o.policy_status().and_then(|s| s.is_public())));
    let tags = tagging.map_err(AwsError::from_sdk).map(|o| {
        Value::Object(
            o.tag_set()
                .iter()
                .map(|t| (t.key().to_owned(), json!(t.value())))
                .collect(),
        )
    });

    let fields = json!({
        "Name": bucket,
        "Versioning": versioning.status().map(|s| s.as_str()).unwrap_or("Disabled"),
        "MfaDelete": versioning.mfa_delete().map(|s| s.as_str()),
        "Encryption": or_error(encryption, json!("none")),
        "PublicAccessBlock": or_error(public_access, json!("not configured")),
        "PolicyIsPublic": or_error(is_public, json!("no bucket policy")),
        "Tags": or_error(tags, json!({})),
    });
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::error::ErrorMetadata;
    use aws_sdk_s3::operation::get_bucket_encryption::GetBucketEncryptionError;
    use aws_sdk_s3::operation::get_bucket_policy_status::GetBucketPolicyStatusOutput;
    use aws_sdk_s3::operation::get_bucket_tagging::GetBucketTaggingError;
    use aws_sdk_s3::operation::get_bucket_versioning::GetBucketVersioningOutput;
    use aws_sdk_s3::operation::get_public_access_block::GetPublicAccessBlockOutput;
    use aws_sdk_s3::operation::list_buckets::ListBucketsOutput;
    use aws_sdk_s3::types::{BucketVersioningStatus, PolicyStatus, PublicAccessBlockConfiguration};
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    #[tokio::test]
    async fn buckets_carry_their_region_and_drill_into_objects() {
        let rule = mock!(Client::list_buckets)
            .match_requests(|r| {
                r.max_buckets() == Some(1000) && r.continuation_token() == Some("t1")
            })
            .then_output(|| {
                ListBucketsOutput::builder()
                    .buckets(
                        Bucket::builder()
                            .name("logs")
                            .bucket_region("eu-west-1")
                            .build(),
                    )
                    .continuation_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_s3, [&rule]);
        let page = list(
            &client,
            &ListQuery {
                parent: None,
                next: Some("t1".into()),
            },
        )
        .await
        .unwrap();

        assert_eq!(page.next.as_deref(), Some("t2"));
        let row = &page.rows[0];
        assert_eq!(row.region.as_deref(), Some("eu-west-1"));
        let drill = row.drill.as_ref().unwrap();
        assert_eq!(drill.kind, "s3:object");
        assert_eq!(drill.parent.id, "logs/");
        assert_eq!(drill.parent.region.as_deref(), Some("eu-west-1"));
    }

    #[tokio::test]
    async fn describe_reports_settings_and_keeps_going_past_missing_ones() {
        let versioning = mock!(Client::get_bucket_versioning).then_output(|| {
            GetBucketVersioningOutput::builder()
                .status(BucketVersioningStatus::Enabled)
                .build()
        });
        let encryption = mock!(Client::get_bucket_encryption).then_error(|| {
            GetBucketEncryptionError::generic(ErrorMetadata::builder().code("AccessDenied").build())
        });
        let public = mock!(Client::get_public_access_block).then_output(|| {
            GetPublicAccessBlockOutput::builder()
                .public_access_block_configuration(
                    PublicAccessBlockConfiguration::builder()
                        .block_public_acls(true)
                        .build(),
                )
                .build()
        });
        let policy = mock!(Client::get_bucket_policy_status).then_output(|| {
            GetBucketPolicyStatusOutput::builder()
                .policy_status(PolicyStatus::builder().is_public(false).build())
                .build()
        });
        let tagging = mock!(Client::get_bucket_tagging).then_error(|| {
            GetBucketTaggingError::generic(ErrorMetadata::builder().code("NoSuchTagSet").build())
        });
        let client = mock_client!(
            aws_sdk_s3,
            RuleMode::MatchAny,
            [&versioning, &encryption, &public, &policy, &tagging]
        );

        let d = describe(&client, "logs").await.unwrap();
        assert_eq!(d.fields["Versioning"], "Enabled");
        assert_eq!(d.fields["PublicAccessBlock"]["BlockPublicAcls"], true);
        assert_eq!(d.fields["PolicyIsPublic"], false);
        assert!(
            d.fields["Encryption"]
                .as_str()
                .unwrap()
                .contains("AccessDenied")
        );
        assert_eq!(d.fields["Tags"], json!({}), "no tags is not an error");
    }
}
