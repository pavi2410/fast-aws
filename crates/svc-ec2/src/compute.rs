//! `ec2:volume`, `ec2:ami`, `ec2:keypair` and `ec2:eip`.

use async_trait::async_trait;
use aws_sdk_ec2::Client;
use aws_sdk_ec2::types::{Address, Image, KeyPairInfo, Volume};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

use crate::{name, tags};

pub struct Volumes;

const VOLUME_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("ID", 21),
    Column::fixed("SIZE", 8),
    Column::fixed("TYPE", 8),
    Column::fixed("STATE", 10),
    Column::fixed("ATTACHED TO", 19),
    Column::fixed("AZ", 12),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Volumes {
    kind_info! {
        id: "ec2:volume",
        title: "EBS Volumes",
        aliases: ["vol", "volumes", "ebs"],
        scope: Scope::Regional,
        columns: VOLUME_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_volumes(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_volumes()
            .volume_ids(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let v = out
            .volumes()
            .first()
            .ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "VolumeId": v.volume_id(),
            "Name": name(v.tags()),
            "SizeGiB": v.size(),
            "VolumeType": v.volume_type().map(|t| t.as_str()),
            "Iops": v.iops(),
            "Throughput": v.throughput(),
            "State": v.state().map(|s| s.as_str()),
            "Encrypted": v.encrypted(),
            "KmsKeyId": v.kms_key_id(),
            "SnapshotId": v.snapshot_id(),
            "AvailabilityZone": v.availability_zone(),
            "CreateTime": v.create_time().map(|t| t.to_string()),
            "Attachments": v.attachments().iter().map(|a| json!({
                "InstanceId": a.instance_id(),
                "Device": a.device(),
                "State": a.state().map(|s| s.as_str()),
                "DeleteOnTermination": a.delete_on_termination(),
            })).collect::<Vec<_>>(),
            "Tags": tags(v.tags()),
        });
        Ok(Detail {
            fields,
            raw: format!("{v:#?}"),
        })
    }
}

async fn list_volumes(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .describe_volumes()
        .max_results(500)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .volumes()
        .iter()
        .map(|v| {
            let instance = v
                .attachments()
                .first()
                .and_then(|a| a.instance_id())
                .unwrap_or_default();
            volume_row(v).relate(format!("instance {instance}"), "ec2:instance", instance)
        })
        .collect();
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

fn volume_row(v: &Volume) -> Row {
    let state = v.state().map(|s| s.as_str()).unwrap_or("-");
    let tone = match state {
        "in-use" => Tone::Good,
        "available" => Tone::Warn, // unattached: often forgotten and still billed
        "error" => Tone::Bad,
        _ => Tone::Normal,
    };
    Row::new(
        v.volume_id().unwrap_or_default(),
        vec![
            or_dash(name(v.tags())).into(),
            or_dash(v.volume_id()).into(),
            v.size()
                .map(|s| format!("{s} GiB"))
                .unwrap_or_default()
                .into(),
            or_dash(v.volume_type().map(|t| t.as_str())).into(),
            Cell::toned(state, tone),
            or_dash(v.attachments().first().and_then(|a| a.instance_id())).into(),
            or_dash(v.availability_zone()).into(),
            age_of(v.create_time().map(|t| t.secs())).into(),
        ],
    )
}

pub struct Images;

const IMAGE_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("ID", 21),
    Column::fixed("STATE", 9),
    Column::fixed("ARCH", 7),
    Column::fixed("PUBLIC", 6),
    Column::fixed("CREATED", 10),
];

#[async_trait]
impl ResourceKind for Images {
    kind_info! {
        id: "ec2:ami",
        title: "AMIs (owned)",
        aliases: ["ami", "amis", "images"],
        scope: Scope::Regional,
        columns: IMAGE_COLUMNS,
    }

    /// Only images this account owns; the public catalogue is far too large to list.
    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_images(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_images()
            .image_ids(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let i = out
            .images()
            .first()
            .ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "ImageId": i.image_id(),
            "Name": i.name(),
            "Description": i.description(),
            "State": i.state().map(|s| s.as_str()),
            "Architecture": i.architecture().map(|a| a.as_str()),
            "PlatformDetails": i.platform_details(),
            "RootDeviceType": i.root_device_type().map(|t| t.as_str()),
            "Public": i.public(),
            "CreationDate": i.creation_date(),
            "Snapshots": i.block_device_mappings().iter()
                .filter_map(|m| m.ebs().and_then(|e| e.snapshot_id()))
                .collect::<Vec<_>>(),
            "Tags": tags(i.tags()),
        });
        Ok(Detail {
            fields,
            raw: format!("{i:#?}"),
        })
    }
}

async fn list_images(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .describe_images()
        .owners("self")
        .max_results(1000)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out.images().iter().map(image_row).collect();
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

fn image_row(i: &Image) -> Row {
    let public = i.public() == Some(true);
    Row::new(
        i.image_id().unwrap_or_default(),
        vec![
            or_dash(i.name()).into(),
            or_dash(i.image_id()).into(),
            or_dash(i.state().map(|s| s.as_str())).into(),
            or_dash(i.architecture().map(|a| a.as_str())).into(),
            if public {
                Cell::toned("yes", Tone::Warn)
            } else {
                Cell::new("")
            },
            // An ISO 8601 timestamp; the date is enough here.
            or_dash(i.creation_date().map(|d| d.get(..10).unwrap_or(d))).into(),
        ],
    )
}

pub struct KeyPairs;

const KEY_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("ID", 21),
    Column::fixed("TYPE", 7),
    Column::fill("FINGERPRINT", 3),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for KeyPairs {
    kind_info! {
        id: "ec2:keypair",
        title: "Key Pairs",
        aliases: ["kp", "keypairs", "keys"],
        scope: Scope::Regional,
        columns: KEY_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .describe_key_pairs()
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out.key_pairs().iter().map(key_row).collect(),
            next: None,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_key_pairs()
            .key_pair_ids(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let k = out
            .key_pairs()
            .first()
            .ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "KeyPairId": k.key_pair_id(),
            "KeyName": k.key_name(),
            "KeyType": k.key_type().map(|t| t.as_str()),
            "KeyFingerprint": k.key_fingerprint(),
            "CreateTime": k.create_time().map(|t| t.to_string()),
            "Tags": tags(k.tags()),
        });
        Ok(Detail {
            fields,
            raw: format!("{k:#?}"),
        })
    }
}

fn key_row(k: &KeyPairInfo) -> Row {
    Row::new(
        k.key_pair_id().unwrap_or_default(),
        vec![
            or_dash(k.key_name()).into(),
            or_dash(k.key_pair_id()).into(),
            or_dash(k.key_type().map(|t| t.as_str())).into(),
            or_dash(k.key_fingerprint()).into(),
            age_of(k.create_time().map(|t| t.secs())).into(),
        ],
    )
}

pub struct Addresses;

const EIP_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("PUBLIC IP", 15),
    Column::fixed("ALLOCATION ID", 26),
    Column::fixed("INSTANCE", 19),
    Column::fixed("PRIVATE IP", 15),
];

#[async_trait]
impl ResourceKind for Addresses {
    kind_info! {
        id: "ec2:eip",
        title: "Elastic IPs",
        aliases: ["eip", "eips", "addresses"],
        scope: Scope::Regional,
        columns: EIP_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .describe_addresses()
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out
                .addresses()
                .iter()
                .map(|a| {
                    let instance = a.instance_id().unwrap_or_default();
                    eip_row(a).relate(format!("instance {instance}"), "ec2:instance", instance)
                })
                .collect(),
            next: None,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_addresses()
            .allocation_ids(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let a = out
            .addresses()
            .first()
            .ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "AllocationId": a.allocation_id(),
            "PublicIp": a.public_ip(),
            "Name": name(a.tags()),
            "InstanceId": a.instance_id(),
            "NetworkInterfaceId": a.network_interface_id(),
            "PrivateIpAddress": a.private_ip_address(),
            "AssociationId": a.association_id(),
            "Domain": a.domain().map(|d| d.as_str()),
            "Tags": tags(a.tags()),
        });
        Ok(Detail {
            fields,
            raw: format!("{a:#?}"),
        })
    }
}

fn eip_row(a: &Address) -> Row {
    let instance = match a.instance_id() {
        Some(i) => Cell::new(i),
        // Unassociated addresses are billed hourly.
        None if a.association_id().is_none() => Cell::toned("unattached", Tone::Warn),
        None => Cell::new("-"),
    };
    Row::new(
        a.allocation_id().unwrap_or_default(),
        vec![
            or_dash(name(a.tags())).into(),
            or_dash(a.public_ip()).into(),
            or_dash(a.allocation_id()).into(),
            instance,
            or_dash(a.private_ip_address()).into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_ec2::operation::describe_images::DescribeImagesOutput;
    use aws_sdk_ec2::operation::describe_volumes::DescribeVolumesOutput;
    use aws_sdk_ec2::types::{VolumeAttachment, VolumeState};
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn volumes_show_attachment_and_flag_unattached() {
        let rule = mock!(Client::describe_volumes)
            .match_requests(|r| r.next_token() == Some("t1"))
            .then_output(|| {
                DescribeVolumesOutput::builder()
                    .volumes(
                        Volume::builder()
                            .volume_id("vol-1")
                            .size(8)
                            .state(VolumeState::InUse)
                            .attachments(VolumeAttachment::builder().instance_id("i-1").build())
                            .build(),
                    )
                    .volumes(
                        Volume::builder()
                            .volume_id("vol-2")
                            .state(VolumeState::Available)
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_ec2, [&rule]);
        let page = list_volumes(
            &client,
            &ListQuery {
                parent: None,
                next: Some("t1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.next.as_deref(), Some("t2"));
        assert_eq!(page.rows[0].cells[2].text, "8 GiB");
        assert_eq!(page.rows[0].cells[5].text, "i-1");
        assert_eq!(page.rows[1].cells[4].tone, Tone::Warn);
    }

    #[tokio::test]
    async fn images_are_limited_to_the_account() {
        let rule = mock!(Client::describe_images)
            .match_requests(|r| r.owners() == ["self".to_owned()])
            .then_output(|| {
                DescribeImagesOutput::builder()
                    .images(
                        Image::builder()
                            .image_id("ami-1")
                            .name("base")
                            .public(true)
                            .build(),
                    )
                    .build()
            });
        let client = mock_client!(aws_sdk_ec2, [&rule]);
        let page = list_images(&client, &ListQuery::default()).await.unwrap();
        assert_eq!(page.rows[0].cells[0].text, "base");
        assert_eq!(page.rows[0].cells[4].tone, Tone::Warn);
        assert_eq!(rule.num_calls(), 1);
    }

    #[test]
    fn unattached_elastic_ips_are_flagged() {
        let eip = Address::builder()
            .allocation_id("eipalloc-1")
            .public_ip("1.2.3.4")
            .build();
        assert_eq!(eip_row(&eip).cells[3].tone, Tone::Warn);
        let used = Address::builder()
            .allocation_id("eipalloc-2")
            .instance_id("i-1")
            .build();
        assert_eq!(eip_row(&used).cells[3].text, "i-1");
    }
}
