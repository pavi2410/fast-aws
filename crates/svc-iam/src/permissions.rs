//! `iam:permission`: a role's or user's effective permissions, every statement
//! of every policy attached to it in one table. Reached by drilling into a role
//! or user.
//!
//! Only IAM List/Get calls, at most [`CONCURRENCY`] at a time (IAM's rate limits
//! are low).

use async_trait::async_trait;
use aws_sdk_iam::Client;
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use futures_util::stream::{self, StreamExt};
use serde_json::{Value, json};

use crate::policy_doc::decode_document;

const CONCURRENCY: usize = 4;
/// Separates the parts of a statement row's ID.
const SEP: char = '\u{1f}';

pub struct Permissions;

const COLUMNS: &[Column] = &[
    Column::fixed("EFFECT", 6),
    Column::fill("ACTIONS", 4),
    Column::fill("RESOURCES", 3),
    Column::fixed("CONDITION", 9),
    Column::fill("POLICY", 2),
];

/// Whose permissions a list shows: a parent ID of `role:<name>` or `user:<name>`.
pub(crate) fn parent_for(principal: &str, name: &str) -> Parent {
    Parent {
        id: format!("{principal}:{name}"),
        label: name.to_owned(),
        region: None,
    }
}

#[async_trait]
impl ResourceKind for Permissions {
    kind_info! {
        id: "iam:permission",
        title: "Effective Permissions",
        aliases: ["permissions"],
        scope: Scope::Global,
        columns: COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("role or user"))?;
        let policies = policies_of(&Client::new(ctx.config()), &parent.id).await?;
        Ok(Page {
            rows: rows(&policies),
            next: None,
        })
    }

    /// The row holds its statement; no request.
    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let mut parts = id.splitn(3, SEP);
        let policy = parts.next().unwrap_or_default();
        let statement: Value = parts
            .nth(1)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let fields = json!({ "Policy": policy, "Statement": statement });
        Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        })
    }
}

/// A policy's name (with `inline` or `managed`) and its decoded document.
pub(crate) struct NamedPolicy {
    pub name: String,
    pub document: Value,
}

/// Every policy of a role or user: inline ones, then attached managed ones at
/// their default version.
async fn policies_of(client: &Client, parent: &str) -> Result<Vec<NamedPolicy>, AwsError> {
    let (principal, name) = parent.split_once(':').unwrap_or(("role", parent));
    let (inline_names, managed_arns): (Vec<String>, Vec<String>) = if principal == "user" {
        let (inline, managed) = tokio::join!(
            client.list_user_policies().user_name(name).send(),
            client.list_attached_user_policies().user_name(name).send(),
        );
        let inline = inline.map_err(AwsError::from_sdk)?.policy_names().to_vec();
        let managed = managed.map_err(AwsError::from_sdk)?;
        (
            inline,
            managed
                .attached_policies()
                .iter()
                .filter_map(|p| p.policy_arn().map(str::to_owned))
                .collect(),
        )
    } else {
        let (inline, managed) = tokio::join!(
            client.list_role_policies().role_name(name).send(),
            client.list_attached_role_policies().role_name(name).send(),
        );
        let inline = inline.map_err(AwsError::from_sdk)?.policy_names().to_vec();
        let managed = managed.map_err(AwsError::from_sdk)?;
        (
            inline,
            managed
                .attached_policies()
                .iter()
                .filter_map(|p| p.policy_arn().map(str::to_owned))
                .collect(),
        )
    };

    let jobs = inline_names
        .into_iter()
        .map(Fetch::Inline)
        .chain(managed_arns.into_iter().map(Fetch::Managed));
    let results: Vec<(String, Result<String, AwsError>)> = stream::iter(jobs)
        .map(|job| fetch(client, principal, name, job))
        .buffered(CONCURRENCY)
        .collect()
        .await;

    Ok(results
        .into_iter()
        .map(|(name, document)| NamedPolicy {
            name,
            document: match document {
                Ok(encoded) => decode_document(&encoded),
                // One unreadable policy shouldn't hide the others.
                Err(e) => json!({ "Error": e.to_string() }),
            },
        })
        .collect())
}

/// One policy document to fetch.
enum Fetch {
    /// An inline policy, by name.
    Inline(String),
    /// A managed policy, by ARN, at its default version.
    Managed(String),
}

/// Fetches one policy's URL-encoded document, with the name to show for it.
async fn fetch(
    client: &Client,
    principal: &str,
    name: &str,
    job: Fetch,
) -> (String, Result<String, AwsError>) {
    match job {
        Fetch::Inline(policy) => {
            let document = if principal == "user" {
                client
                    .get_user_policy()
                    .user_name(name)
                    .policy_name(&policy)
                    .send()
                    .await
                    .map(|o| o.policy_document().to_owned())
                    .map_err(AwsError::from_sdk)
            } else {
                client
                    .get_role_policy()
                    .role_name(name)
                    .policy_name(&policy)
                    .send()
                    .await
                    .map(|o| o.policy_document().to_owned())
                    .map_err(AwsError::from_sdk)
            };
            (format!("{policy} (inline)"), document)
        }
        Fetch::Managed(arn) => {
            let short = arn.rsplit('/').next().unwrap_or(&arn).to_owned();
            let document = async {
                let policy = client
                    .get_policy()
                    .policy_arn(&arn)
                    .send()
                    .await
                    .map_err(AwsError::from_sdk)?;
                let version = policy
                    .policy()
                    .and_then(|p| p.default_version_id())
                    .unwrap_or("v1")
                    .to_owned();
                let v = client
                    .get_policy_version()
                    .policy_arn(&arn)
                    .version_id(version)
                    .send()
                    .await
                    .map_err(AwsError::from_sdk)?;
                Ok(v.policy_version()
                    .and_then(|v| v.document())
                    .unwrap_or_default()
                    .to_owned())
            }
            .await;
            (short, document)
        }
    }
}

/// A policy value that may be a string or a list.
fn list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// One row per statement, policy by policy.
pub(crate) fn rows(policies: &[NamedPolicy]) -> Vec<Row> {
    let mut rows = Vec::new();
    for policy in policies {
        if let Some(error) = policy.document.get("Error").and_then(Value::as_str) {
            let cells = vec![
                Cell::toned("?", Tone::Warn),
                Cell::toned(format!("⚠ couldn't read: {error}"), Tone::Warn),
                "".into(),
                "".into(),
                policy.name.as_str().into(),
            ];
            rows.push(Row::new(
                format!("{}{SEP}error{SEP}null", policy.name),
                cells,
            ));
            continue;
        }
        let statements = match &policy.document["Statement"] {
            Value::Array(list) => list.clone(),
            single @ Value::Object(_) => vec![single.clone()],
            _ => Vec::new(),
        };
        for (i, s) in statements.iter().enumerate() {
            let effect = s["Effect"].as_str().unwrap_or("?");
            let (actions, not_actions) = (list(&s["Action"]), list(&s["NotAction"]));
            let actions = if not_actions.is_empty() {
                actions.join(", ")
            } else {
                format!("NOT {}", not_actions.join(", "))
            };
            let (resources, not_resources) = (list(&s["Resource"]), list(&s["NotResource"]));
            let resources = if not_resources.is_empty() {
                resources.join(", ")
            } else {
                format!("NOT {}", not_resources.join(", "))
            };
            let effect_cell = if effect == "Deny" {
                Cell::toned("Deny", Tone::Bad)
            } else {
                Cell::toned(effect, Tone::Good)
            };
            let condition = if s.get("Condition").is_some() {
                "yes"
            } else {
                ""
            };
            rows.push(Row::new(
                format!("{}{SEP}{i}{SEP}{s}", policy.name),
                vec![
                    effect_cell,
                    actions.into(),
                    resources.into(),
                    condition.into(),
                    policy.name.as_str().into(),
                ],
            ));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_iam::operation::get_policy::GetPolicyOutput;
    use aws_sdk_iam::operation::get_policy_version::GetPolicyVersionOutput;
    use aws_sdk_iam::operation::get_role_policy::GetRolePolicyOutput;
    use aws_sdk_iam::operation::list_attached_role_policies::ListAttachedRolePoliciesOutput;
    use aws_sdk_iam::operation::list_role_policies::ListRolePoliciesOutput;
    use aws_sdk_iam::types::{AttachedPolicy, Policy, PolicyVersion};
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    #[test]
    fn statements_become_rows() {
        let policies = [NamedPolicy {
            name: "logs (inline)".into(),
            document: json!({"Statement": [
                {"Effect": "Allow", "Action": ["logs:CreateLogStream", "logs:PutLogEvents"], "Resource": "*"},
                {"Effect": "Deny", "NotAction": "s3:Get*", "Resource": "arn:aws:s3:::x/*", "Condition": {"Bool": {"aws:SecureTransport": "false"}}},
            ]}),
        }];
        let rows = rows(&policies);
        let text = |r: &Row| r.cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
        assert_eq!(
            text(&rows[0]),
            [
                "Allow",
                "logs:CreateLogStream, logs:PutLogEvents",
                "*",
                "",
                "logs (inline)"
            ]
        );
        assert_eq!(
            text(&rows[1]),
            [
                "Deny",
                "NOT s3:Get*",
                "arn:aws:s3:::x/*",
                "yes",
                "logs (inline)"
            ]
        );
        assert_eq!(rows[1].cells[0].tone, Tone::Bad);
        assert!(
            rows[0].matches("logs:put"),
            "the / filter finds statements by action"
        );
    }

    #[tokio::test]
    async fn a_roles_inline_and_managed_policies_are_gathered() {
        let inline_names = mock!(Client::list_role_policies).then_output(|| {
            ListRolePoliciesOutput::builder()
                .policy_names("logs")
                .build()
                .unwrap()
        });
        let attached = mock!(Client::list_attached_role_policies).then_output(|| {
            ListAttachedRolePoliciesOutput::builder()
                .attached_policies(
                    AttachedPolicy::builder()
                        .policy_arn("arn:aws:iam::aws:policy/ReadOnlyAccess")
                        .build(),
                )
                .build()
        });
        let inline_doc = mock!(Client::get_role_policy).then_output(|| {
            GetRolePolicyOutput::builder()
                .role_name("r")
                .policy_name("logs")
                .policy_document("%7B%22Statement%22%3A%5B%7B%22Effect%22%3A%22Allow%22%2C%22Action%22%3A%22logs%3A%2A%22%2C%22Resource%22%3A%22%2A%22%7D%5D%7D")
                .build()
                .unwrap()
        });
        let policy = mock!(Client::get_policy).then_output(|| {
            GetPolicyOutput::builder()
                .policy(Policy::builder().default_version_id("v9").build())
                .build()
        });
        let version = mock!(Client::get_policy_version)
            .match_requests(|r| r.version_id() == Some("v9"))
            .then_output(|| {
                GetPolicyVersionOutput::builder()
                    .policy_version(
                        PolicyVersion::builder()
                            .document("%7B%22Statement%22%3A%7B%22Effect%22%3A%22Allow%22%2C%22Action%22%3A%22%2A%3AGet%2A%22%2C%22Resource%22%3A%22%2A%22%7D%7D")
                            .build(),
                    )
                    .build()
            });
        let client = mock_client!(
            aws_sdk_iam,
            RuleMode::MatchAny,
            [&inline_names, &attached, &inline_doc, &policy, &version]
        );
        let policies = policies_of(&client, "role:r").await.unwrap();
        let names: Vec<_> = policies.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["logs (inline)", "ReadOnlyAccess"]);
        let rows = rows(&policies);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].cells[1].text, "*:Get*");
    }
}
