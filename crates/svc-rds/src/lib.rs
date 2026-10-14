//! RDS resource kinds: databases (Aurora clusters and DB instances in one list)
//! and snapshots (instance and cluster snapshots in one list).
//!
//! Each list walks two APIs in turn; the page token says which one is next.

use async_trait::async_trait;
use aws_sdk_rds::Client;
use aws_sdk_rds::types::{DbCluster, DbClusterSnapshot, DbInstance, DbSnapshot, Tag};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::metrics::{MetricSpec, Summary};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

/// Registers every RDS resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Databases);
    registry.register(Snapshots);
}

/// Which of the two APIs a token continues, and from where (`None` = start).
fn split_token(token: Option<&str>, first: &str, second: &str) -> (bool, Option<String>) {
    match token {
        None => (true, None),
        Some(t) => match t.split_once(':') {
            Some((k, rest)) if k == first => {
                (true, Some(rest.to_owned()).filter(|r| !r.is_empty()))
            }
            Some((k, rest)) if k == second => {
                (false, Some(rest.to_owned()).filter(|r| !r.is_empty()))
            }
            _ => (true, None),
        },
    }
}

/// The token after a page of the first API: more of it, or start the second.
fn after_first(first: &str, second: &str, marker: Option<&str>) -> Option<String> {
    Some(match marker {
        Some(m) => format!("{first}:{m}"),
        None => format!("{second}:"),
    })
}

fn status_cell(status: Option<&str>) -> Cell {
    let status = status.unwrap_or("-");
    let tone = match status {
        "available" => Tone::Good,
        "stopped" => Tone::Dim,
        "failed" | "incompatible-parameters" | "incompatible-network" | "storage-full" => Tone::Bad,
        _ => Tone::Warn,
    };
    Cell::toned(status, tone)
}

fn tags(tags: &[Tag]) -> Value {
    Value::Object(
        tags.iter()
            .filter_map(|t| Some((t.key()?.to_owned(), json!(t.value()))))
            .collect(),
    )
}

pub struct Databases;

const DB_COLUMNS: &[Column] = &[
    Column::fill("NAME", 2),
    Column::fixed("ENGINE", 22),
    Column::fixed("CLASS", 15),
    Column::fixed("STATUS", 12),
    Column::fill("ROLE", 2),
    Column::fill("ENDPOINT", 3),
    Column::spark("CPU 24h (peak)", 20, "CPU 6h"),
    Column::spark("CONNS 24h (peak)", 18, "CONNS 6h"),
];

#[async_trait]
impl ResourceKind for Databases {
    kind_info! {
        id: "rds:db",
        title: "RDS Databases",
        aliases: ["rds", "db", "aurora", "databases"],
        scope: Scope::Regional,
        columns: DB_COLUMNS,
    }

    /// Clusters first, then instances (`cl:` / `in:` tokens).
    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_databases(&Client::new(ctx.config()), query.next.as_deref()).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let client = Client::new(ctx.config());
        match id.split_once(':') {
            Some(("cluster", name)) => describe_cluster(&client, name).await,
            Some((_, name)) => describe_instance(&client, name).await,
            None => describe_instance(&client, id).await,
        }
    }
}

async fn list_databases(client: &Client, token: Option<&str>) -> Result<Page, AwsError> {
    let (clusters, marker) = split_token(token, "cl", "in");
    if clusters {
        let out = client
            .describe_db_clusters()
            .max_records(100)
            .set_marker(marker)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let rows = out
            .db_clusters()
            .iter()
            .map(|c| cluster_relations(cluster_row(c), c))
            .collect();
        return Ok(Page {
            rows,
            next: after_first("cl", "in", out.marker()),
        });
    }
    let out = client
        .describe_db_instances()
        .max_records(100)
        .set_marker(marker)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .db_instances()
        .iter()
        .map(|i| instance_relations(instance_row(i), i))
        .collect();
    Ok(Page {
        rows,
        next: out.marker().map(|m| format!("in:{m}")),
    })
}

fn engine(engine: Option<&str>, version: Option<&str>) -> String {
    match (engine, version) {
        (Some(e), Some(v)) => format!("{e} {v}"),
        (Some(e), None) => e.to_owned(),
        _ => "-".into(),
    }
}

fn cluster_row(c: &DbCluster) -> Row {
    let name = c.db_cluster_identifier().unwrap_or_default();
    let members = c.db_cluster_members().len();
    Row::new(
        format!("cluster:{name}"),
        vec![
            name.into(),
            engine(c.engine(), c.engine_version()).into(),
            "-".into(),
            status_cell(c.status()),
            format!("cluster ({members} instances)").into(),
            or_dash(c.endpoint()).into(),
            "".into(),
            "".into(),
        ],
    )
}

fn instance_row(i: &DbInstance) -> Row {
    let name = i.db_instance_identifier().unwrap_or_default();
    let role = match i.db_cluster_identifier() {
        Some(cluster) => format!("in {cluster}"),
        None if i.read_replica_source_db_instance_identifier().is_some() => "read replica".into(),
        None if i.multi_az() == Some(true) => "standalone, multi-AZ".into(),
        None => "standalone".into(),
    };
    let endpoint = i
        .endpoint()
        .and_then(|e| Some(format!("{}:{}", e.address()?, e.port()?)))
        .unwrap_or_else(|| "-".into());
    Row::new(
        format!("instance:{name}"),
        vec![
            name.into(),
            engine(i.engine(), i.engine_version()).into(),
            or_dash(i.db_instance_class()).into(),
            status_cell(i.db_instance_status()),
            role.into(),
            endpoint.into(),
            "".into(),
            "".into(),
        ],
    )
    .metric(MetricSpec {
        namespace: "AWS/RDS",
        name: "CPUUtilization",
        dimensions: vec![("DBInstanceIdentifier", name.to_owned())],
        stat: "Average",
        summary: Summary::Peak,
        unit: "%",
        column: 6,
    })
    // The hourly maximum: an average hides short bursts of connections.
    .metric(MetricSpec {
        namespace: "AWS/RDS",
        name: "DatabaseConnections",
        dimensions: vec![("DBInstanceIdentifier", name.to_owned())],
        stat: "Maximum",
        summary: Summary::Peak,
        unit: "",
        column: 7,
    })
}

async fn describe_cluster(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let out = client
        .describe_db_clusters()
        .db_cluster_identifier(name)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let c = out
        .db_clusters()
        .first()
        .ok_or_else(|| AwsError::not_found(name))?;
    let fields = json!({
        "Identifier": c.db_cluster_identifier(),
        "Arn": c.db_cluster_arn(),
        "Engine": engine(c.engine(), c.engine_version()),
        "Status": c.status(),
        "Endpoint": c.endpoint(),
        "ReaderEndpoint": c.reader_endpoint(),
        "Port": c.port(),
        "Members": c.db_cluster_members().iter().map(|m| json!({
            "Instance": m.db_instance_identifier(),
            "Role": if m.is_cluster_writer() == Some(true) { "writer" } else { "reader" },
        })).collect::<Vec<_>>(),
        "DatabaseName": c.database_name(),
        "MasterUsername": c.master_username(),
        "MasterUserSecretArn": c.master_user_secret().and_then(|s| s.secret_arn()),
        "IamDatabaseAuthentication": c.iam_database_authentication_enabled(),
        "StorageEncrypted": c.storage_encrypted(),
        "KmsKeyId": c.kms_key_id(),
        "BackupRetentionPeriod": c.backup_retention_period(),
        "DeletionProtection": c.deletion_protection(),
        "SecurityGroups": c.vpc_security_groups().iter().map(|g| g.vpc_security_group_id()).collect::<Vec<_>>(),
        "SubnetGroup": c.db_subnet_group(),
        "ParameterGroup": c.db_cluster_parameter_group(),
        "Created": c.cluster_create_time().map(|t| t.to_string()),
        "Tags": tags(c.tag_list()),
    });
    Ok(Detail {
        fields,
        raw: format!("{c:#?}"),
    })
}

async fn describe_instance(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let out = client
        .describe_db_instances()
        .db_instance_identifier(name)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let i = out
        .db_instances()
        .first()
        .ok_or_else(|| AwsError::not_found(name))?;
    let fields = json!({
        "Identifier": i.db_instance_identifier(),
        "Arn": i.db_instance_arn(),
        "Engine": engine(i.engine(), i.engine_version()),
        "Class": i.db_instance_class(),
        "Status": i.db_instance_status(),
        "Cluster": i.db_cluster_identifier(),
        "Endpoint": i.endpoint().map(|e| json!({ "Address": e.address(), "Port": e.port() })),
        "AvailabilityZone": i.availability_zone(),
        "MultiAZ": i.multi_az(),
        "PubliclyAccessible": i.publicly_accessible(),
        "Storage": json!({
            "Type": i.storage_type(),
            "AllocatedGiB": i.allocated_storage(),
            "Iops": i.iops(),
            "Encrypted": i.storage_encrypted(),
            "KmsKeyId": i.kms_key_id(),
        }),
        "DatabaseName": i.db_name(),
        "MasterUsername": i.master_username(),
        "MasterUserSecretArn": i.master_user_secret().and_then(|s| s.secret_arn()),
        "IamDatabaseAuthentication": i.iam_database_authentication_enabled(),
        "BackupRetentionPeriod": i.backup_retention_period(),
        "DeletionProtection": i.deletion_protection(),
        "ReadReplicaSource": i.read_replica_source_db_instance_identifier(),
        "ReadReplicas": i.read_replica_db_instance_identifiers(),
        "SecurityGroups": i.vpc_security_groups().iter().map(|g| g.vpc_security_group_id()).collect::<Vec<_>>(),
        "SubnetGroup": i.db_subnet_group().and_then(|g| g.db_subnet_group_name()),
        "ParameterGroups": i.db_parameter_groups().iter().map(|g| g.db_parameter_group_name()).collect::<Vec<_>>(),
        "Created": i.instance_create_time().map(|t| t.to_string()),
        "Tags": tags(i.tag_list()),
    });
    Ok(Detail {
        fields,
        raw: format!("{i:#?}"),
    })
}

pub struct Snapshots;

const SNAPSHOT_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fill("SOURCE", 2),
    Column::fixed("TYPE", 9),
    Column::fixed("ENGINE", 16),
    Column::fixed("STATUS", 10),
    Column::fixed("SIZE", 8),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Snapshots {
    kind_info! {
        id: "rds:snapshot",
        title: "RDS Snapshots",
        aliases: ["snap", "snapshots", "rdssnap"],
        scope: Scope::Regional,
        columns: SNAPSHOT_COLUMNS,
    }

    /// Cluster snapshots first, then instance snapshots (`cs:` / `is:`).
    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_snapshots(&Client::new(ctx.config()), query.next.as_deref()).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let client = Client::new(ctx.config());
        if let Some(name) = id.strip_prefix("cluster:") {
            let out = client
                .describe_db_cluster_snapshots()
                .db_cluster_snapshot_identifier(name)
                .send()
                .await
                .map_err(AwsError::from_sdk)?;
            let s = out
                .db_cluster_snapshots()
                .first()
                .ok_or_else(|| AwsError::not_found(name))?;
            let fields = json!({
                "Identifier": s.db_cluster_snapshot_identifier(),
                "Arn": s.db_cluster_snapshot_arn(),
                "Cluster": s.db_cluster_identifier(),
                "Type": s.snapshot_type(),
                "Engine": engine(s.engine(), s.engine_version()),
                "Status": s.status(),
                "AllocatedGiB": s.allocated_storage(),
                "Encrypted": s.storage_encrypted(),
                "KmsKeyId": s.kms_key_id(),
                "Created": s.snapshot_create_time().map(|t| t.to_string()),
                "Tags": tags(s.tag_list()),
            });
            return Ok(Detail {
                fields,
                raw: format!("{s:#?}"),
            });
        }
        let name = id.strip_prefix("instance:").unwrap_or(id);
        let out = client
            .describe_db_snapshots()
            .db_snapshot_identifier(name)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let s = out
            .db_snapshots()
            .first()
            .ok_or_else(|| AwsError::not_found(name))?;
        let fields = json!({
            "Identifier": s.db_snapshot_identifier(),
            "Arn": s.db_snapshot_arn(),
            "Instance": s.db_instance_identifier(),
            "Type": s.snapshot_type(),
            "Engine": engine(s.engine(), s.engine_version()),
            "Status": s.status(),
            "AllocatedGiB": s.allocated_storage(),
            "Encrypted": s.encrypted(),
            "KmsKeyId": s.kms_key_id(),
            "Created": s.snapshot_create_time().map(|t| t.to_string()),
            "Tags": tags(s.tag_list()),
        });
        Ok(Detail {
            fields,
            raw: format!("{s:#?}"),
        })
    }
}

async fn list_snapshots(client: &Client, token: Option<&str>) -> Result<Page, AwsError> {
    let (clusters, marker) = split_token(token, "cs", "is");
    if clusters {
        let out = client
            .describe_db_cluster_snapshots()
            .max_records(100)
            .set_marker(marker)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let rows = out
            .db_cluster_snapshots()
            .iter()
            .map(cluster_snapshot_row)
            .collect();
        return Ok(Page {
            rows,
            next: after_first("cs", "is", out.marker()),
        });
    }
    let out = client
        .describe_db_snapshots()
        .max_records(100)
        .set_marker(marker)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out
        .db_snapshots()
        .iter()
        .map(instance_snapshot_row)
        .collect();
    Ok(Page {
        rows,
        next: out.marker().map(|m| format!("is:{m}")),
    })
}

/// The columns shared by instance and cluster snapshots.
struct SnapshotCells<'a> {
    name: Option<&'a str>,
    source: Option<&'a str>,
    kind: Option<&'a str>,
    engine: Option<&'a str>,
    status: Option<&'a str>,
    size_gib: Option<i32>,
    created: Option<i64>,
}

impl SnapshotCells<'_> {
    fn row(self, id: String) -> Row {
        Row::new(
            id,
            vec![
                or_dash(self.name).into(),
                or_dash(self.source).into(),
                or_dash(self.kind).into(),
                or_dash(self.engine).into(),
                status_cell(self.status),
                self.size_gib
                    .map(|s| format!("{s} GiB"))
                    .unwrap_or_default()
                    .into(),
                age_of(self.created).into(),
            ],
        )
    }
}

fn cluster_snapshot_row(s: &DbClusterSnapshot) -> Row {
    let name = s.db_cluster_snapshot_identifier();
    SnapshotCells {
        name,
        source: s.db_cluster_identifier(),
        kind: s.snapshot_type(),
        engine: s.engine(),
        status: s.status(),
        size_gib: s.allocated_storage(),
        created: s.snapshot_create_time().map(|t| t.secs()),
    }
    .row(format!("cluster:{}", name.unwrap_or_default()))
}

fn instance_snapshot_row(s: &DbSnapshot) -> Row {
    let name = s.db_snapshot_identifier();
    SnapshotCells {
        name,
        source: s.db_instance_identifier(),
        kind: s.snapshot_type(),
        engine: s.engine(),
        status: s.status(),
        size_gib: s.allocated_storage(),
        created: s.snapshot_create_time().map(|t| t.secs()),
    }
    .row(format!("instance:{}", name.unwrap_or_default()))
}

/// What `g` offers from a cluster: its instances, master secret and groups.
fn cluster_relations(mut row: Row, c: &DbCluster) -> Row {
    let name = c.db_cluster_identifier().unwrap_or_default();
    // Instance rows say "in <cluster>".
    row = row.relate("its instances", "rds:db", &format!("in {name}"));
    if let Some(secret) = c.master_user_secret().and_then(|s| s.secret_arn()) {
        row = row.relate("master user secret", "secrets:secret", secret);
    }
    for g in c.vpc_security_groups() {
        let id = g.vpc_security_group_id().unwrap_or_default();
        row = row.relate(format!("security group {id}"), "ec2:sg", id);
    }
    row
}

/// What `g` offers from an instance: its cluster, master secret and groups.
fn instance_relations(mut row: Row, i: &DbInstance) -> Row {
    if let Some(cluster) = i.db_cluster_identifier() {
        row = row.relate(format!("cluster {cluster}"), "rds:db", cluster);
    }
    if let Some(secret) = i.master_user_secret().and_then(|s| s.secret_arn()) {
        row = row.relate("master user secret", "secrets:secret", secret);
    }
    for g in i.vpc_security_groups() {
        let id = g.vpc_security_group_id().unwrap_or_default();
        row = row.relate(format!("security group {id}"), "ec2:sg", id);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_rds::operation::describe_db_clusters::DescribeDbClustersOutput;
    use aws_sdk_rds::operation::describe_db_instances::DescribeDbInstancesOutput;
    use aws_sdk_rds::types::{DbClusterMember, Endpoint};
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn tokens_name_the_api_to_continue() {
        assert_eq!(split_token(None, "cl", "in"), (true, None));
        assert_eq!(
            split_token(Some("cl:m1"), "cl", "in"),
            (true, Some("m1".into()))
        );
        assert_eq!(split_token(Some("in:"), "cl", "in"), (false, None));
        assert_eq!(
            split_token(Some("in:m2"), "cl", "in"),
            (false, Some("m2".into()))
        );
        assert_eq!(after_first("cl", "in", Some("m")), Some("cl:m".into()));
        assert_eq!(after_first("cl", "in", None), Some("in:".into()));
    }

    #[test]
    fn members_lead_to_their_cluster_and_secret() {
        let i = DbInstance::builder()
            .db_instance_identifier("orders-1")
            .db_cluster_identifier("orders")
            .master_user_secret(
                aws_sdk_rds::types::MasterUserSecret::builder()
                    .secret_arn("arn:aws:secretsmanager:x:1:secret:rds!db-1")
                    .build(),
            )
            .build();
        let row = instance_relations(instance_row(&i), &i);
        let got: Vec<_> = row
            .relations
            .iter()
            .map(|r| (r.kind, r.filter.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("rds:db", Some("orders")),
                (
                    "secrets:secret",
                    Some("arn:aws:secretsmanager:x:1:secret:rds!db-1")
                )
            ]
        );
        let c = DbCluster::builder().db_cluster_identifier("orders").build();
        assert_eq!(
            cluster_relations(cluster_row(&c), &c).relations[0]
                .filter
                .as_deref(),
            Some("in orders")
        );
    }

    #[tokio::test]
    async fn clusters_then_instances() {
        let clusters = mock!(Client::describe_db_clusters).then_output(|| {
            DescribeDbClustersOutput::builder()
                .db_clusters(
                    DbCluster::builder()
                        .db_cluster_identifier("orders")
                        .engine("aurora-postgresql")
                        .engine_version("16.4")
                        .status("available")
                        .db_cluster_members(
                            DbClusterMember::builder()
                                .db_instance_identifier("orders-1")
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        let instances = mock!(Client::describe_db_instances).then_output(|| {
            DescribeDbInstancesOutput::builder()
                .db_instances(
                    DbInstance::builder()
                        .db_instance_identifier("orders-1")
                        .db_cluster_identifier("orders")
                        .db_instance_status("available")
                        .build(),
                )
                .db_instances(
                    DbInstance::builder()
                        .db_instance_identifier("legacy")
                        .engine("mysql")
                        .db_instance_status("stopped")
                        .endpoint(
                            Endpoint::builder()
                                .address("legacy.x.rds.amazonaws.com")
                                .port(3306)
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        let client = mock_client!(
            aws_sdk_rds,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&clusters, &instances]
        );

        let first = list_databases(&client, None).await.unwrap();
        assert_eq!(first.rows[0].id, "cluster:orders");
        assert_eq!(first.rows[0].cells[1].text, "aurora-postgresql 16.4");
        assert_eq!(first.rows[0].cells[4].text, "cluster (1 instances)");
        assert_eq!(first.next.as_deref(), Some("in:"));

        let second = list_databases(&client, first.next.as_deref())
            .await
            .unwrap();
        assert_eq!(second.rows[0].cells[4].text, "in orders");
        assert_eq!(second.rows[1].cells[3].tone, Tone::Dim);
        assert_eq!(
            second.rows[1].cells[5].text,
            "legacy.x.rds.amazonaws.com:3306"
        );
        assert!(first.rows[0].metrics.is_empty(), "clusters have none");
        let metrics: Vec<_> = second.rows[1]
            .metrics
            .iter()
            .map(|m| (m.name, m.column))
            .collect();
        assert_eq!(metrics, [("CPUUtilization", 6), ("DatabaseConnections", 7)]);
        assert!(
            second
                .rows
                .iter()
                .all(|r| r.cells.len() == DB_COLUMNS.len())
        );
        assert!(first.rows.iter().all(|r| r.cells.len() == DB_COLUMNS.len()));
        assert_eq!(second.next, None);
    }
}
