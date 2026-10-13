//! IAM resource kinds: roles, users, groups and customer-managed policies.
//!
//! IAM is global; the runtime sends these calls to `us-east-1`.

mod permissions;
mod policy_doc;
mod principals;
mod simulation;

use async_trait::async_trait;
use aws_sdk_iam::Client;
use aws_sdk_iam::types::{AttachedPolicy, Group, Policy, PolicyScopeType, Role, Tag, User};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, kind_info,
};
use serde_json::{Value, json};

use policy_doc::decode_document;
use principals::trusted_by;

/// Registers every IAM resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Roles);
    registry.register(Users);
    registry.register(Groups);
    registry.register(Policies);
    registry.register(permissions::Permissions);
    registry.register(simulation::Simulations);
}

fn tags(tags: &[Tag]) -> Value {
    Value::Object(
        tags.iter()
            .map(|t| (t.key().to_owned(), json!(t.value())))
            .collect(),
    )
}

fn attached(policies: &[AttachedPolicy]) -> Value {
    json!(
        policies
            .iter()
            .filter_map(|p| p.policy_name())
            .collect::<Vec<_>>()
    )
}

/// A sub-call's result for a describe view: the value, or the error inline, so
/// one denied call doesn't hide the rest.
fn or_error<T>(r: Result<T, AwsError>, f: impl FnOnce(T) -> Value) -> Value {
    r.map(f).unwrap_or_else(|e| json!(format!("⚠ {e}")))
}

pub struct Roles;

const ROLE_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fill("TRUSTED BY", 3),
    Column::fixed("PATH", 14),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Roles {
    kind_info! {
        id: "iam:role",
        title: "IAM Roles",
        aliases: ["role", "roles", "iam"],
        scope: Scope::Global,
        columns: ROLE_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list_roles(&Client::new(ctx.config()), query).await
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        _: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        describe_role(&Client::new(ctx.config()), name).await
    }
}

async fn list_roles(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .list_roles()
        .max_items(100)
        .set_marker(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out
            .roles()
            .iter()
            .map(|r| {
                role_row(r)
                    .drill(
                        "iam:permission",
                        permissions::parent_for("role", r.role_name()),
                    )
                    .query(
                        'S',
                        "simulate",
                        simulation::PROMPT,
                        "iam:simulation",
                        r.arn(),
                    )
            })
            .collect(),
        next: out.marker,
    })
}

fn role_row(r: &Role) -> Row {
    let trust = r
        .assume_role_policy_document()
        .map(decode_document)
        .unwrap_or(Value::Null);
    Row::new(
        r.role_name(),
        vec![
            r.role_name().into(),
            trusted_by(&trust).join(", ").into(),
            r.path().into(),
            age_of(Some(r.create_date().secs())).into(),
        ],
    )
}

async fn describe_role(client: &Client, name: &str) -> Result<Detail, AwsError> {
    let (role, managed, inline) = tokio::join!(
        client.get_role().role_name(name).send(),
        client.list_attached_role_policies().role_name(name).send(),
        client.list_role_policies().role_name(name).send(),
    );
    let role = role.map_err(AwsError::from_sdk)?;
    let r = role.role().ok_or_else(|| AwsError::not_found(name))?;
    let trust = r
        .assume_role_policy_document()
        .map(decode_document)
        .unwrap_or(Value::Null);
    let last_used = r.role_last_used();
    let fields = json!({
        "RoleName": r.role_name(),
        "Arn": r.arn(),
        "Description": r.description(),
        "Path": r.path(),
        "TrustedBy": trusted_by(&trust),
        "LastUsed": last_used.map(|u| json!({
            "Date": u.last_used_date().map(|d| d.to_string()),
            "Region": u.region(),
        })),
        "MaxSessionDuration": r.max_session_duration(),
        "PermissionsBoundary": r.permissions_boundary().and_then(|b| b.permissions_boundary_arn()),
        "ManagedPolicies": or_error(managed.map_err(AwsError::from_sdk), |o| attached(o.attached_policies())),
        "InlinePolicies": or_error(inline.map_err(AwsError::from_sdk), |o| json!(o.policy_names())),
        "TrustPolicy": trust,
        "CreateDate": r.create_date().to_string(),
        "Tags": tags(r.tags()),
    });
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

pub struct Users;

const USER_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("CONSOLE LAST USED", 17),
    Column::fixed("PATH", 14),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Users {
    kind_info! {
        id: "iam:user",
        title: "IAM Users",
        aliases: ["user", "users"],
        scope: Scope::Global,
        columns: USER_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .list_users()
            .max_items(100)
            .set_marker(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out
                .users()
                .iter()
                .map(|u| {
                    user_row(u)
                        .drill(
                            "iam:permission",
                            permissions::parent_for("user", u.user_name()),
                        )
                        .query(
                            'S',
                            "simulate",
                            simulation::PROMPT,
                            "iam:simulation",
                            u.arn(),
                        )
                })
                .collect(),
            next: out.marker,
        })
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        _: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        let client = Client::new(ctx.config());
        let (user, keys, mfa, groups, managed) = tokio::join!(
            client.get_user().user_name(name).send(),
            client.list_access_keys().user_name(name).send(),
            client.list_mfa_devices().user_name(name).send(),
            client.list_groups_for_user().user_name(name).send(),
            client.list_attached_user_policies().user_name(name).send(),
        );
        let user = user.map_err(AwsError::from_sdk)?;
        let u = user.user().ok_or_else(|| AwsError::not_found(name))?;
        let fields = json!({
            "UserName": u.user_name(),
            "Arn": u.arn(),
            "Path": u.path(),
            "CreateDate": u.create_date().to_string(),
            "PasswordLastUsed": u.password_last_used().map(|d| d.to_string()),
            "AccessKeys": or_error(keys.map_err(AwsError::from_sdk), |o| json!(
                o.access_key_metadata().iter().map(|k| json!({
                    "AccessKeyId": k.access_key_id(),
                    "Status": k.status().map(|s| s.as_str()),
                    "Age": age_of(k.create_date().map(|d| d.secs())),
                })).collect::<Vec<_>>()
            )),
            "MfaDevices": or_error(mfa.map_err(AwsError::from_sdk), |o| json!(
                o.mfa_devices().iter().map(|d| d.serial_number()).collect::<Vec<_>>()
            )),
            "Groups": or_error(groups.map_err(AwsError::from_sdk), |o| json!(
                o.groups().iter().map(|g| g.group_name()).collect::<Vec<_>>()
            )),
            "ManagedPolicies": or_error(managed.map_err(AwsError::from_sdk), |o| attached(o.attached_policies())),
            "PermissionsBoundary": u.permissions_boundary().and_then(|b| b.permissions_boundary_arn()),
            "Tags": tags(u.tags()),
        });
        Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        })
    }
}

fn user_row(u: &User) -> Row {
    Row::new(
        u.user_name(),
        vec![
            u.user_name().into(),
            u.password_last_used()
                .map(|d| age_of(Some(d.secs())) + " ago")
                .unwrap_or_else(|| "never".into())
                .into(),
            u.path().into(),
            age_of(Some(u.create_date().secs())).into(),
        ],
    )
}

pub struct Groups;

const GROUP_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("PATH", 14),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Groups {
    kind_info! {
        id: "iam:group",
        title: "IAM Groups",
        aliases: ["group", "groups"],
        scope: Scope::Global,
        columns: GROUP_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .list_groups()
            .max_items(100)
            .set_marker(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out.groups().iter().map(group_row).collect(),
            next: out.marker,
        })
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        _: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        let client = Client::new(ctx.config());
        let (group, managed) = tokio::join!(
            client.get_group().group_name(name).send(),
            client
                .list_attached_group_policies()
                .group_name(name)
                .send(),
        );
        let group = group.map_err(AwsError::from_sdk)?;
        let fields = json!({
            "GroupName": name,
            "Arn": group.group().map(|g| g.arn()),
            "Members": group.users().iter().map(|u| u.user_name()).collect::<Vec<_>>(),
            "ManagedPolicies": or_error(managed.map_err(AwsError::from_sdk), |o| attached(o.attached_policies())),
        });
        Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        })
    }
}

fn group_row(g: &Group) -> Row {
    Row::new(
        g.group_name(),
        vec![
            g.group_name().into(),
            g.path().into(),
            age_of(Some(g.create_date().secs())).into(),
        ],
    )
}

pub struct Policies;

const POLICY_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("ATTACHED", 8),
    Column::fixed("VERSION", 7),
    Column::fixed("UPDATED", 7),
    Column::fill("DESCRIPTION", 3),
];

#[async_trait]
impl ResourceKind for Policies {
    kind_info! {
        id: "iam:policy",
        title: "IAM Policies (customer)",
        aliases: ["policy", "policies"],
        scope: Scope::Global,
        columns: POLICY_COLUMNS,
    }

    /// Customer-managed policies only; the AWS-managed catalogue is over a
    /// thousand entries nobody browses.
    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .list_policies()
            .scope(PolicyScopeType::Local)
            .max_items(100)
            .set_marker(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out.policies().iter().map(policy_row).collect(),
            next: out.marker,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, arn: &str) -> Result<Detail, AwsError> {
        describe_policy(&Client::new(ctx.config()), arn).await
    }
}

fn policy_row(p: &Policy) -> Row {
    Row::new(
        p.arn().unwrap_or_default(),
        vec![
            or_dash(p.policy_name()).into(),
            p.attachment_count().unwrap_or_default().to_string().into(),
            or_dash(p.default_version_id()).into(),
            age_of(p.update_date().map(|d| d.secs())).into(),
            or_dash(p.description()).into(),
        ],
    )
}

async fn describe_policy(client: &Client, arn: &str) -> Result<Detail, AwsError> {
    let policy = client
        .get_policy()
        .policy_arn(arn)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let p = policy.policy().ok_or_else(|| AwsError::not_found(arn))?;
    let version = p.default_version_id().unwrap_or("v1");
    let document = client
        .get_policy_version()
        .policy_arn(arn)
        .version_id(version)
        .send()
        .await
        .map_err(AwsError::from_sdk)
        .map(|o| {
            o.policy_version()
                .and_then(|v| v.document())
                .map(decode_document)
                .unwrap_or(Value::Null)
        });
    let fields = json!({
        "PolicyName": p.policy_name(),
        "Arn": p.arn(),
        "Description": p.description(),
        "DefaultVersion": version,
        "AttachmentCount": p.attachment_count(),
        "UpdateDate": p.update_date().map(|d| d.to_string()),
        "Document": or_error(document, |d| d),
        "Tags": tags(p.tags()),
    });
    Ok(Detail {
        raw: format!("{fields:#}"),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_iam::operation::get_policy::GetPolicyOutput;
    use aws_sdk_iam::operation::get_policy_version::GetPolicyVersionOutput;
    use aws_sdk_iam::operation::list_roles::ListRolesOutput;
    use aws_sdk_iam::primitives::DateTime;
    use aws_sdk_iam::types::PolicyVersion;
    use aws_smithy_mocks::{RuleMode, mock, mock_client};

    const LAMBDA_TRUST: &str = "%7B%22Version%22%3A%222012-10-17%22%2C%22Statement%22%3A%5B%7B%22Effect%22%3A%22Allow%22%2C%22Principal%22%3A%7B%22Service%22%3A%22lambda.amazonaws.com%22%7D%2C%22Action%22%3A%22sts%3AAssumeRole%22%7D%5D%7D";

    #[tokio::test]
    async fn roles_show_who_can_assume_them() {
        let rule = mock!(Client::list_roles)
            .match_requests(|r| r.marker() == Some("m1"))
            .then_output(|| {
                ListRolesOutput::builder()
                    .roles(
                        Role::builder()
                            .role_name("thumbs-exec")
                            .path("/")
                            .role_id("AROA1")
                            .arn("arn:aws:iam::1:role/thumbs-exec")
                            .create_date(DateTime::from_secs(0))
                            .assume_role_policy_document(LAMBDA_TRUST)
                            .build()
                            .unwrap(),
                    )
                    .marker("m2")
                    .is_truncated(true)
                    .build()
                    .unwrap()
            });
        let client = mock_client!(aws_sdk_iam, [&rule]);
        let page = list_roles(
            &client,
            &ListQuery {
                parent: None,
                next: Some("m1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.next.as_deref(), Some("m2"));
        assert_eq!(page.rows[0].cells[1].text, "lambda");
    }

    #[tokio::test]
    async fn policy_describe_decodes_the_default_version_document() {
        let policy = mock!(Client::get_policy).then_output(|| {
            GetPolicyOutput::builder()
                .policy(
                    Policy::builder()
                        .policy_name("read-logs")
                        .arn("arn:aws:iam::1:policy/read-logs")
                        .default_version_id("v3")
                        .build(),
                )
                .build()
        });
        let version = mock!(Client::get_policy_version)
            .match_requests(|r| r.version_id() == Some("v3"))
            .then_output(|| {
                GetPolicyVersionOutput::builder()
                    .policy_version(
                        PolicyVersion::builder()
                            .document("%7B%22Statement%22%3A%5B%7B%22Action%22%3A%22logs%3AGet%2A%22%7D%5D%7D")
                            .build(),
                    )
                    .build()
            });
        let client = mock_client!(aws_sdk_iam, RuleMode::MatchAny, [&policy, &version]);
        let d = describe_policy(&client, "arn:aws:iam::1:policy/read-logs")
            .await
            .unwrap();
        assert_eq!(d.fields["Document"]["Statement"][0]["Action"], "logs:Get*");
        assert_eq!(d.fields["DefaultVersion"], "v3");
    }
}
