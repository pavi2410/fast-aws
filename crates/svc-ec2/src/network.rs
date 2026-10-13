//! `ec2:sg`, `ec2:vpc` and `ec2:subnet`: security groups and VPC networking.

use async_trait::async_trait;
use aws_sdk_ec2::Client;
use aws_sdk_ec2::types::{Filter, IpPermission, SecurityGroup, Subnet, Vpc};
use fast_aws_core::fmt::or_dash;
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

use crate::{name, tags};

pub struct SecurityGroups;

const SG_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("ID", 20),
    Column::fixed("VPC", 21),
    Column::fixed("IN", 3),
    Column::fixed("OUT", 3),
    Column::fill("DESCRIPTION", 3),
];

#[async_trait]
impl ResourceKind for SecurityGroups {
    kind_info! {
        id: "ec2:sg",
        title: "Security Groups",
        aliases: ["sg", "sgs", "securitygroups"],
        scope: Scope::Regional,
        columns: SG_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_sgs(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_security_groups()
            .group_ids(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let sg = out
            .security_groups()
            .first()
            .ok_or_else(|| AwsError::not_found(id))?;
        Ok(Detail {
            fields: sg_fields(sg),
            raw: format!("{sg:#?}"),
        })
    }
}

async fn list_sgs(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .describe_security_groups()
        .max_results(1000)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out.security_groups().iter().map(sg_row).collect();
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

fn sg_row(sg: &SecurityGroup) -> Row {
    let id = sg.group_id().unwrap_or_default();
    Row::new(
        id,
        vec![
            or_dash(sg.group_name()).into(),
            id.into(),
            or_dash(sg.vpc_id()).into(),
            sg.ip_permissions().len().to_string().into(),
            sg.ip_permissions_egress().len().to_string().into(),
            or_dash(sg.description()).into(),
        ],
    )
}

/// One rule as `protocol ports ← sources`, the way people read them.
fn rule(p: &IpPermission) -> Value {
    let protocol = match p.ip_protocol() {
        Some("-1") | None => "all".to_owned(),
        Some(other) => other.to_owned(),
    };
    let ports = match (p.from_port(), p.to_port()) {
        (Some(-1), _) | (None, _) => "all".to_owned(),
        (Some(from), Some(to)) if from != to => format!("{from}-{to}"),
        (Some(from), _) => from.to_string(),
    };
    let mut sources: Vec<String> = Vec::new();
    sources.extend(
        p.ip_ranges()
            .iter()
            .filter_map(|r| r.cidr_ip().map(str::to_owned)),
    );
    sources.extend(
        p.ipv6_ranges()
            .iter()
            .filter_map(|r| r.cidr_ipv6().map(str::to_owned)),
    );
    sources.extend(
        p.user_id_group_pairs()
            .iter()
            .filter_map(|g| g.group_id().map(str::to_owned)),
    );
    sources.extend(
        p.prefix_list_ids()
            .iter()
            .filter_map(|l| l.prefix_list_id().map(str::to_owned)),
    );
    json!({ "Protocol": protocol, "Ports": ports, "Peers": sources })
}

fn sg_fields(sg: &SecurityGroup) -> Value {
    json!({
        "GroupId": sg.group_id(),
        "GroupName": sg.group_name(),
        "Description": sg.description(),
        "VpcId": sg.vpc_id(),
        "OwnerId": sg.owner_id(),
        "Inbound": sg.ip_permissions().iter().map(rule).collect::<Vec<_>>(),
        "Outbound": sg.ip_permissions_egress().iter().map(rule).collect::<Vec<_>>(),
        "Tags": tags(sg.tags()),
    })
}

pub struct Vpcs;

const VPC_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("ID", 21),
    Column::fixed("CIDR", 18),
    Column::fixed("STATE", 9),
    Column::fixed("DEFAULT", 7),
];

#[async_trait]
impl ResourceKind for Vpcs {
    kind_info! {
        id: "ec2:vpc",
        title: "VPCs",
        aliases: ["vpc", "vpcs"],
        scope: Scope::Regional,
        columns: VPC_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .describe_vpcs()
            .max_results(1000)
            .set_next_token(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let rows = out.vpcs().iter().map(vpc_row).collect();
        Ok(Page {
            rows,
            next: out.next_token,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .describe_vpcs()
            .vpc_ids(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let vpc = out.vpcs().first().ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "VpcId": vpc.vpc_id(),
            "Name": name(vpc.tags()),
            "CidrBlock": vpc.cidr_block(),
            "State": vpc.state().map(|s| s.as_str()),
            "IsDefault": vpc.is_default(),
            "DhcpOptionsId": vpc.dhcp_options_id(),
            "InstanceTenancy": vpc.instance_tenancy().map(|t| t.as_str()),
            "OwnerId": vpc.owner_id(),
            "Tags": tags(vpc.tags()),
        });
        Ok(Detail {
            fields,
            raw: format!("{vpc:#?}"),
        })
    }
}

fn vpc_row(vpc: &Vpc) -> Row {
    let id = vpc.vpc_id().unwrap_or_default();
    let label = name(vpc.tags()).unwrap_or(id).to_owned();
    Row::new(
        id,
        vec![
            or_dash(name(vpc.tags())).into(),
            id.into(),
            or_dash(vpc.cidr_block()).into(),
            or_dash(vpc.state().map(|s| s.as_str())).into(),
            if vpc.is_default() == Some(true) {
                "yes"
            } else {
                ""
            }
            .into(),
        ],
    )
    .drill(
        "ec2:subnet",
        Parent {
            id: id.to_owned(),
            label,
            region: None,
        },
    )
}

pub struct Subnets;

const SUBNET_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("ID", 24),
    Column::fixed("VPC", 21),
    Column::fixed("CIDR", 18),
    Column::fixed("AZ", 12),
    Column::fixed("FREE IPS", 8),
    Column::fixed("PUBLIC", 6),
];

#[async_trait]
impl ResourceKind for Subnets {
    kind_info! {
        id: "ec2:subnet",
        title: "Subnets",
        aliases: ["subnet", "subnets"],
        scope: Scope::Regional,
        columns: SUBNET_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_subnets(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        describe_subnet(&Client::new(ctx.config()), id).await
    }
}

/// All subnets, or only those of the parent VPC when drilled into from one.
async fn list_subnets(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let vpc_filter = query
        .parent
        .as_ref()
        .map(|vpc| Filter::builder().name("vpc-id").values(&vpc.id).build());
    let out = client
        .describe_subnets()
        .set_filters(vpc_filter.map(|f| vec![f]))
        .max_results(1000)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out.subnets().iter().map(subnet_row).collect();
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

async fn describe_subnet(client: &Client, id: &str) -> Result<Detail, AwsError> {
    let out = client
        .describe_subnets()
        .subnet_ids(id)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let s = out
        .subnets()
        .first()
        .ok_or_else(|| AwsError::not_found(id))?;
    let fields = json!({
        "SubnetId": s.subnet_id(),
        "Name": name(s.tags()),
        "VpcId": s.vpc_id(),
        "CidrBlock": s.cidr_block(),
        "AvailabilityZone": s.availability_zone(),
        "AvailableIpAddressCount": s.available_ip_address_count(),
        "MapPublicIpOnLaunch": s.map_public_ip_on_launch(),
        "DefaultForAz": s.default_for_az(),
        "State": s.state().map(|s| s.as_str()),
        "Tags": tags(s.tags()),
    });
    Ok(Detail {
        fields,
        raw: format!("{s:#?}"),
    })
}

fn subnet_row(s: &Subnet) -> Row {
    let public = s.map_public_ip_on_launch() == Some(true);
    Row::new(
        s.subnet_id().unwrap_or_default(),
        vec![
            or_dash(name(s.tags())).into(),
            or_dash(s.subnet_id()).into(),
            or_dash(s.vpc_id()).into(),
            or_dash(s.cidr_block()).into(),
            or_dash(s.availability_zone()).into(),
            s.available_ip_address_count()
                .map(|n| n.to_string())
                .unwrap_or_default()
                .into(),
            if public {
                Cell::toned("yes", Tone::Warn)
            } else {
                Cell::new("")
            },
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput;
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsOutput;
    use aws_sdk_ec2::types::{IpRange, Tag, UserIdGroupPair};
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn rules_read_like_the_console() {
        let ssh = IpPermission::builder()
            .ip_protocol("tcp")
            .from_port(22)
            .to_port(22)
            .ip_ranges(IpRange::builder().cidr_ip("10.0.0.0/8").build())
            .build();
        assert_eq!(
            rule(&ssh),
            json!({"Protocol": "tcp", "Ports": "22", "Peers": ["10.0.0.0/8"]})
        );
        let all = IpPermission::builder()
            .ip_protocol("-1")
            .user_id_group_pairs(UserIdGroupPair::builder().group_id("sg-1").build())
            .build();
        assert_eq!(
            rule(&all),
            json!({"Protocol": "all", "Ports": "all", "Peers": ["sg-1"]})
        );
        let range = IpPermission::builder()
            .ip_protocol("udp")
            .from_port(1000)
            .to_port(2000)
            .build();
        assert_eq!(rule(&range)["Ports"], "1000-2000");
    }

    #[test]
    fn vpc_rows_drill_into_their_subnets() {
        let vpc = Vpc::builder()
            .vpc_id("vpc-1")
            .cidr_block("10.0.0.0/16")
            .tags(Tag::builder().key("Name").value("main").build())
            .build();
        let drill = vpc_row(&vpc).drill.unwrap();
        assert_eq!(drill.kind, "ec2:subnet");
        assert_eq!(drill.parent.id, "vpc-1");
        assert_eq!(drill.parent.label, "main");
    }

    #[tokio::test]
    async fn subnets_inside_a_vpc_are_filtered_to_it() {
        let rule = mock!(Client::describe_subnets)
            .match_requests(|r| {
                r.filters().first().is_some_and(|f| {
                    f.name() == Some("vpc-id") && f.values() == ["vpc-1".to_owned()]
                })
            })
            .then_output(|| {
                DescribeSubnetsOutput::builder()
                    .subnets(
                        Subnet::builder()
                            .subnet_id("subnet-1")
                            .vpc_id("vpc-1")
                            .map_public_ip_on_launch(true)
                            .available_ip_address_count(250)
                            .build(),
                    )
                    .build()
            });
        let client = mock_client!(aws_sdk_ec2, [&rule]);
        let parent = Parent {
            id: "vpc-1".into(),
            label: "main".into(),
            region: None,
        };
        let query = ListQuery {
            parent: Some(parent),
            next: None,
        };

        let page = list_subnets(&client, &query).await.unwrap();
        assert_eq!(page.rows[0].id, "subnet-1");
        assert_eq!(page.rows[0].cells[5].text, "250");
        assert_eq!(page.rows[0].cells[6].tone, Tone::Warn);
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn security_groups_page_and_count_rules() {
        let rule = mock!(Client::describe_security_groups)
            .match_requests(|r| r.next_token() == Some("t1"))
            .then_output(|| {
                DescribeSecurityGroupsOutput::builder()
                    .security_groups(
                        SecurityGroup::builder()
                            .group_id("sg-1")
                            .group_name("web")
                            .ip_permissions(IpPermission::builder().ip_protocol("tcp").build())
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_ec2, [&rule]);
        let query = ListQuery {
            parent: None,
            next: Some("t1".into()),
        };

        let page = list_sgs(&client, &query).await.unwrap();
        assert_eq!(page.next.as_deref(), Some("t2"));
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(cells, ["web", "sg-1", "-", "1", "0", "-"]);
    }
}
