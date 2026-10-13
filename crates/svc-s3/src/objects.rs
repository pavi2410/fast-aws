//! `s3:object`: the objects in a bucket, one `/` level at a time. Only reachable
//! by drilling into a bucket.

use async_trait::async_trait;
use aws_sdk_s3::Client;
use fast_aws_core::fmt::{age_of, bytes, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

use crate::{parent_id, split_parent};

pub struct Objects;

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 4),
    Column::fixed("SIZE", 10),
    Column::fixed("MODIFIED", 8),
    Column::fixed("CLASS", 12),
];

#[async_trait]
impl ResourceKind for Objects {
    kind_info! {
        id: "s3:object",
        title: "S3 Objects",
        aliases: ["objects"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("bucket"))?;
        list(&Client::new(ctx.config()), parent, query.next.clone()).await
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        key: &str,
    ) -> Result<Detail, AwsError> {
        let parent = parent.ok_or_else(|| AwsError::not_found("bucket"))?;
        describe(&Client::new(ctx.config()), split_parent(&parent.id).0, key).await
    }
}

async fn list(client: &Client, parent: &Parent, next: Option<String>) -> Result<Page, AwsError> {
    let (bucket, prefix) = split_parent(&parent.id);
    let out = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .delimiter("/")
        .max_keys(1000)
        .set_continuation_token(next)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;

    let relative = |key: &str| key.strip_prefix(prefix).unwrap_or(key).to_owned();
    let folders = out
        .common_prefixes()
        .iter()
        .filter_map(|p| p.prefix())
        .map(|p| {
            let child = Parent {
                id: parent_id(bucket, p),
                label: relative(p),
                region: parent.region.clone(),
            };
            Row::new(
                p,
                vec![
                    Cell::toned(relative(p), Tone::Good),
                    "".into(),
                    "".into(),
                    "".into(),
                ],
            )
            .drill("s3:object", child)
        });
    let files = out
        .contents()
        .iter()
        // The zero-byte "folder" object some tools create for the prefix itself.
        .filter(|o| o.key() != Some(prefix))
        .map(|o| {
            let key = o.key().unwrap_or_default();
            Row::new(
                key,
                vec![
                    relative(key).into(),
                    o.size().map(bytes).unwrap_or_default().into(),
                    age_of(o.last_modified().map(|t| t.secs())).into(),
                    or_dash(o.storage_class().map(|c| c.as_str())).into(),
                ],
            )
        });
    let rows = folders.chain(files).collect();
    Ok(Page {
        rows,
        next: out.next_continuation_token,
    })
}

async fn describe(client: &Client, bucket: &str, key: &str) -> Result<Detail, AwsError> {
    if key.ends_with('/') {
        // A prefix isn't an object; there's nothing more to fetch.
        let fields = json!({ "Bucket": bucket, "Prefix": key });
        return Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        });
    }
    let o = client
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let fields = json!({
        "Bucket": bucket,
        "Key": key,
        "ContentLength": o.content_length(),
        "Size": o.content_length().map(bytes),
        "ContentType": o.content_type(),
        "LastModified": o.last_modified().map(|t| t.to_string()),
        "ETag": o.e_tag(),
        "StorageClass": o.storage_class().map(|c| c.as_str()),
        "ServerSideEncryption": o.server_side_encryption().map(|e| e.as_str()),
        "SSEKMSKeyId": o.ssekms_key_id(),
        "VersionId": o.version_id(),
        "CacheControl": o.cache_control(),
        "ContentEncoding": o.content_encoding(),
        "Metadata": o.metadata(),
    });
    Ok(Detail {
        fields,
        raw: format!("{o:#?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Output;
    use aws_sdk_s3::types::{CommonPrefix, Object};
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn lists_one_level_with_folders_first() {
        let rule = mock!(Client::list_objects_v2)
            .match_requests(|r| {
                r.bucket() == Some("logs")
                    && r.prefix() == Some("2026/")
                    && r.delimiter() == Some("/")
                    && r.continuation_token() == Some("t1")
            })
            .then_output(|| {
                ListObjectsV2Output::builder()
                    .common_prefixes(CommonPrefix::builder().prefix("2026/10/").build())
                    .contents(Object::builder().key("2026/").size(0).build())
                    .contents(
                        Object::builder()
                            .key("2026/summary.json")
                            .size(1536)
                            .build(),
                    )
                    .next_continuation_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_s3, [&rule]);
        let parent = Parent {
            id: "logs/2026/".into(),
            label: "2026/".into(),
            region: Some("eu-west-1".into()),
        };

        let page = list(&client, &parent, Some("t1".into())).await.unwrap();
        assert_eq!(page.next.as_deref(), Some("t2"));
        let names: Vec<_> = page.rows.iter().map(|r| r.cells[0].text.as_str()).collect();
        assert_eq!(names, ["10/", "summary.json"]); // the folder marker is skipped

        let folder = page.rows[0].drill.as_ref().unwrap();
        assert_eq!(folder.parent.id, "logs/2026/10/");
        assert_eq!(folder.parent.region.as_deref(), Some("eu-west-1"));
        assert!(page.rows[1].drill.is_none());
        assert_eq!(page.rows[1].id, "2026/summary.json");
        assert_eq!(page.rows[1].cells[1].text, "1.5 KiB");
    }

    #[tokio::test]
    async fn describing_a_prefix_makes_no_call() {
        let client = mock_client!(aws_sdk_s3, []);
        let d = describe(&client, "logs", "2026/").await.unwrap();
        assert_eq!(d.fields["Prefix"], "2026/");
    }
}
