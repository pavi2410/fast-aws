//! `ec2:instance`: EC2 instances.

use async_trait::async_trait;
use aws_sdk_ec2::Client;
use aws_sdk_ec2::types::{Filter, Instance, InstanceStateName, Tag};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, ResourceKind, Row, Scope, Tone,
};
use serde_json::{Value, json};

/// States listed by default. Terminated instances linger for about an hour and are
/// hidden, as the plan describes.
const LISTED_STATES: [&str; 5] = ["pending", "running", "shutting-down", "stopping", "stopped"];

const COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("ID", 19),
    Column::fixed("STATE", 15),
    Column::fixed("TYPE", 13),
    Column::fixed("PRIVATE IP", 15),
    Column::fixed("PUBLIC IP", 15),
    Column::fixed("AZ", 12),
    Column::fixed("AGE", 5),
];

pub struct Instances;

#[async_trait]
impl ResourceKind for Instances {
    fn id(&self) -> &'static str {
        "ec2:instance"
    }

    fn title(&self) -> &'static str {
        "EC2 Instances"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["ec2", "i", "instances"]
    }

    fn scope(&self) -> Scope {
        Scope::Regional
    }

    fn columns(&self) -> &'static [Column] {
        COLUMNS
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, id: &str) -> Result<Detail, AwsError> {
        describe(&Client::new(ctx.config()), id).await
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let state_filter = Filter::builder()
        .name("instance-state-name")
        .set_values(Some(LISTED_STATES.map(String::from).to_vec()))
        .build();
    let out = client
        .describe_instances()
        .filters(state_filter)
        .max_results(500)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;

    let now = crate::now_secs();
    let rows = out
        .reservations()
        .iter()
        .flat_map(|r| r.instances())
        .map(|i| row(i, now))
        .collect();
    Ok(Page {
        rows,
        next: out.next_token,
    })
}

async fn describe(client: &Client, id: &str) -> Result<Detail, AwsError> {
    let out = client
        .describe_instances()
        .instance_ids(id)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let instance = out
        .reservations()
        .iter()
        .flat_map(|r| r.instances())
        .next()
        .ok_or_else(|| {
            AwsError::new(
                fast_aws_core::ErrorKind::NotFound,
                None,
                format!("{id} not found"),
            )
        })?;
    Ok(Detail {
        fields: fields(instance),
        raw: format!("{instance:#?}"),
    })
}

fn row(i: &Instance, now: i64) -> Row {
    let state = i.state().and_then(|s| s.name());
    Row {
        id: i.instance_id().unwrap_or_default().to_owned(),
        cells: vec![
            name(i.tags()).unwrap_or("-").into(),
            i.instance_id().unwrap_or_default().into(),
            state_cell(state),
            i.instance_type().map(|t| t.as_str()).unwrap_or("-").into(),
            i.private_ip_address().unwrap_or("-").into(),
            i.public_ip_address().unwrap_or("-").into(),
            i.placement()
                .and_then(|p| p.availability_zone())
                .unwrap_or("-")
                .into(),
            i.launch_time()
                .map(|t| crate::age(now, t.secs()))
                .unwrap_or_else(|| "-".into())
                .into(),
        ],
    }
}

fn state_cell(state: Option<&InstanceStateName>) -> Cell {
    let Some(state) = state else {
        return Cell::toned("? unknown", Tone::Dim);
    };
    let (glyph, tone) = match state {
        InstanceStateName::Running => ("●", Tone::Good),
        InstanceStateName::Pending | InstanceStateName::Stopping => ("◐", Tone::Warn),
        InstanceStateName::ShuttingDown => ("◐", Tone::Bad),
        InstanceStateName::Stopped => ("○", Tone::Dim),
        InstanceStateName::Terminated => ("✕", Tone::Dim),
        _ => ("?", Tone::Dim),
    };
    Cell::toned(format!("{glyph} {}", state.as_str()), tone)
}

fn name(tags: &[Tag]) -> Option<&str> {
    tags.iter()
        .find(|t| t.key() == Some("Name"))
        .and_then(|t| t.value())
}

fn fields(i: &Instance) -> Value {
    let tags: serde_json::Map<String, Value> = i
        .tags()
        .iter()
        .map(|t| (t.key().unwrap_or_default().to_owned(), json!(t.value())))
        .collect();
    json!({
        "InstanceId": i.instance_id(),
        "Name": name(i.tags()),
        "State": i.state().and_then(|s| s.name()).map(|s| s.as_str()),
        "InstanceType": i.instance_type().map(|t| t.as_str()),
        "ImageId": i.image_id(),
        "LaunchTime": i.launch_time().map(|t| t.to_string()),
        "AvailabilityZone": i.placement().and_then(|p| p.availability_zone()),
        "VpcId": i.vpc_id(),
        "SubnetId": i.subnet_id(),
        "PrivateIpAddress": i.private_ip_address(),
        "PrivateDnsName": i.private_dns_name(),
        "PublicIpAddress": i.public_ip_address(),
        "PublicDnsName": i.public_dns_name(),
        "SecurityGroups": i.security_groups().iter().map(|g| json!({
            "GroupId": g.group_id(),
            "GroupName": g.group_name(),
        })).collect::<Vec<_>>(),
        "IamInstanceProfile": i.iam_instance_profile().and_then(|p| p.arn()),
        "KeyName": i.key_name(),
        "Architecture": i.architecture().map(|a| a.as_str()),
        "PlatformDetails": i.platform_details(),
        "Tags": tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_ec2::error::ErrorMetadata;
    use aws_sdk_ec2::operation::describe_instances::{
        DescribeInstancesError, DescribeInstancesOutput,
    };
    use aws_sdk_ec2::primitives::DateTime;
    use aws_sdk_ec2::types::{InstanceState, InstanceType, Placement, Reservation};
    use aws_smithy_mocks::{mock, mock_client};
    use fast_aws_core::ErrorKind;

    fn instance(id: &str, name: Option<&str>, state: InstanceStateName) -> Instance {
        let mut b = Instance::builder()
            .instance_id(id)
            .state(InstanceState::builder().name(state).build())
            .instance_type(InstanceType::T3Medium)
            .private_ip_address("10.0.1.12")
            .placement(Placement::builder().availability_zone("eu-west-1a").build())
            .launch_time(DateTime::from_secs(crate::now_secs() - 41 * 86_400));
        if let Some(name) = name {
            b = b.tags(Tag::builder().key("Name").value(name).build());
        }
        b.build()
    }

    #[tokio::test]
    async fn list_flattens_reservations_and_passes_the_token_on() {
        let rule = mock!(Client::describe_instances)
            .match_requests(|r| r.next_token() == Some("page-2"))
            .then_output(|| {
                DescribeInstancesOutput::builder()
                    .reservations(
                        Reservation::builder()
                            .instances(instance("i-1", Some("web-1"), InstanceStateName::Running))
                            .instances(instance("i-2", None, InstanceStateName::Stopped))
                            .build(),
                    )
                    .reservations(
                        Reservation::builder()
                            .instances(instance("i-3", Some("db"), InstanceStateName::Pending))
                            .build(),
                    )
                    .next_token("page-3")
                    .build()
            });
        let client = mock_client!(aws_sdk_ec2, [&rule]);

        let query = ListQuery {
            next: Some("page-2".into()),
            ..Default::default()
        };
        let page = list(&client, &query).await.unwrap();

        assert_eq!(page.next.as_deref(), Some("page-3"));
        let ids: Vec<_> = page.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["i-1", "i-2", "i-3"]);

        let first: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            first,
            [
                "web-1",
                "i-1",
                "● running",
                "t3.medium",
                "10.0.1.12",
                "-",
                "eu-west-1a",
                "41d"
            ]
        );
        assert_eq!(page.rows[0].cells[2].tone, Tone::Good);
        assert_eq!(page.rows[1].cells[0].text, "-");
        assert_eq!(page.rows[1].cells[2].tone, Tone::Dim);
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn access_denied_is_classified() {
        let rule = mock!(Client::describe_instances).then_error(|| {
            DescribeInstancesError::generic(
                ErrorMetadata::builder()
                    .code("UnauthorizedOperation")
                    .message("You are not authorized to perform this operation.")
                    .build(),
            )
        });
        let client = mock_client!(aws_sdk_ec2, [&rule]);

        let err = list(&client, &ListQuery::default()).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::AccessDenied);
        assert_eq!(err.code.as_deref(), Some("UnauthorizedOperation"));
    }

    #[tokio::test]
    async fn describe_builds_fields_and_raw() {
        let rule = mock!(Client::describe_instances).then_output(|| {
            DescribeInstancesOutput::builder()
                .reservations(
                    Reservation::builder()
                        .instances(instance("i-1", Some("web-1"), InstanceStateName::Running))
                        .build(),
                )
                .build()
        });
        let client = mock_client!(aws_sdk_ec2, [&rule]);

        let detail = describe(&client, "i-1").await.unwrap();
        assert_eq!(detail.fields["InstanceId"], "i-1");
        assert_eq!(detail.fields["Name"], "web-1");
        assert_eq!(detail.fields["State"], "running");
        assert_eq!(detail.fields["Tags"]["Name"], "web-1");
        assert!(detail.raw.contains("i-1"));
    }

    #[tokio::test]
    async fn describe_of_a_missing_instance_is_not_found() {
        let rule = mock!(Client::describe_instances)
            .then_output(|| DescribeInstancesOutput::builder().build());
        let client = mock_client!(aws_sdk_ec2, [&rule]);

        let err = describe(&client, "i-gone").await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }
}
