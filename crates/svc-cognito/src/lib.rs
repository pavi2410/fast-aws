//! Cognito: user pools (and their users) and identity pools. They are separate
//! AWS APIs (`cognito-idp` and `cognito-identity`).
//!
//! User data is personal data: it's only ever held in memory for display.

use async_trait::async_trait;
use aws_sdk_cognitoidentity::Client as IdentityClient;
use aws_sdk_cognitoidentityprovider::Client as IdpClient;
use aws_sdk_cognitoidentityprovider::types::{
    AttributeType, UserPoolDescriptionType, UserStatusType, UserType,
};
use fast_aws_core::fmt::{age_of, or_dash};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Map, Value, json};

/// Registers every Cognito resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(UserPools);
    registry.register(Users);
    registry.register(IdentityPools);
}

/// The most either list API accepts per call.
const PAGE: i32 = 60;

pub struct UserPools;

const POOL_COLUMNS: &[Column] = &[
    Column::fill("NAME", 3),
    Column::fixed("ID", 22),
    Column::fixed("MODIFIED", 8),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for UserPools {
    kind_info! {
        id: "cognito:userpool",
        title: "Cognito User Pools",
        aliases: ["userpool", "userpools", "up", "cognito"],
        scope: Scope::Regional,
        columns: POOL_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = IdpClient::new(ctx.config())
            .list_user_pools()
            .max_results(PAGE)
            .set_next_token(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: out.user_pools().iter().map(pool_row).collect(),
            next: out.next_token,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = IdpClient::new(ctx.config())
            .describe_user_pool()
            .user_pool_id(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let p = out.user_pool().ok_or_else(|| AwsError::not_found(id))?;
        let triggers = p.lambda_config().map(|l| {
            let pairs = [
                ("PreSignUp", l.pre_sign_up()),
                ("PostConfirmation", l.post_confirmation()),
                ("PreAuthentication", l.pre_authentication()),
                ("PostAuthentication", l.post_authentication()),
                ("PreTokenGeneration", l.pre_token_generation()),
                ("CustomMessage", l.custom_message()),
                ("UserMigration", l.user_migration()),
                ("DefineAuthChallenge", l.define_auth_challenge()),
                ("CreateAuthChallenge", l.create_auth_challenge()),
                (
                    "VerifyAuthChallengeResponse",
                    l.verify_auth_challenge_response(),
                ),
            ];
            let map: Map<String, Value> = pairs
                .into_iter()
                .filter_map(|(k, v)| Some((k.to_owned(), json!(v?))))
                .collect();
            Value::Object(map)
        });
        let fields = json!({
            "Id": p.id(),
            "Name": p.name(),
            "Arn": p.arn(),
            "EstimatedUsers": p.estimated_number_of_users(),
            "Mfa": p.mfa_configuration().map(|m| m.as_str()),
            "UsernameAttributes": p.username_attributes().iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            "AutoVerified": p.auto_verified_attributes().iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            "Domain": p.domain(),
            "CustomDomain": p.custom_domain(),
            "EmailSendingAccount": p.email_configuration().and_then(|e| e.email_sending_account()).map(|a| a.as_str()),
            "EmailSourceArn": p.email_configuration().and_then(|e| e.source_arn()),
            "LambdaTriggers": triggers,
            "DeletionProtection": p.deletion_protection().map(|d| d.as_str()),
            "Tier": p.user_pool_tier().map(|t| t.as_str()),
            "CreationDate": p.creation_date().map(|d| d.to_string()),
            "Tags": p.user_pool_tags(),
        });
        Ok(Detail {
            fields,
            raw: format!("{p:#?}"),
        })
    }
}

fn pool_row(p: &UserPoolDescriptionType) -> Row {
    let id = p.id().unwrap_or_default();
    let label = p.name().unwrap_or(id).to_owned();
    Row::new(
        id,
        vec![
            or_dash(p.name()).into(),
            id.into(),
            age_of(p.last_modified_date().map(|d| d.secs())).into(),
            age_of(p.creation_date().map(|d| d.secs())).into(),
        ],
    )
    .drill(
        "cognito:user",
        Parent {
            id: id.to_owned(),
            label,
            region: None,
        },
    )
}

pub struct Users;

const USER_COLUMNS: &[Column] = &[
    Column::fill("USERNAME", 3),
    Column::fill("EMAIL", 3),
    Column::fixed("STATUS", 22),
    Column::fixed("ENABLED", 7),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Users {
    kind_info! {
        id: "cognito:user",
        title: "Cognito Users",
        aliases: ["cognitousers"],
        scope: Scope::Regional,
        columns: USER_COLUMNS,
        palette: false,
    }

    /// `ListUsers` is rate-limited and pools can be huge; the list pauses after a
    /// few pages like every other.
    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let pool = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("user pool"))?;
        list_users(&IdpClient::new(ctx.config()), &pool.id, query.next.clone()).await
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        username: &str,
    ) -> Result<Detail, AwsError> {
        let pool = parent.ok_or_else(|| AwsError::not_found("user pool"))?;
        let client = IdpClient::new(ctx.config());
        let (user, groups) = tokio::join!(
            client
                .admin_get_user()
                .user_pool_id(&pool.id)
                .username(username)
                .send(),
            client
                .admin_list_groups_for_user()
                .user_pool_id(&pool.id)
                .username(username)
                .send(),
        );
        let u = user.map_err(AwsError::from_sdk)?;
        let groups = match groups {
            Ok(g) => json!(
                g.groups()
                    .iter()
                    .filter_map(|g| g.group_name())
                    .collect::<Vec<_>>()
            ),
            Err(e) => json!(format!("⚠ {}", AwsError::from_sdk(e))),
        };
        let fields = json!({
            "Username": u.username(),
            "Status": u.user_status().map(|s| s.as_str()),
            "Enabled": u.enabled(),
            "Attributes": attributes(u.user_attributes()),
            "Groups": groups,
            "PreferredMfa": u.preferred_mfa_setting(),
            "MfaMethods": u.user_mfa_setting_list(),
            "Created": u.user_create_date().map(|d| d.to_string()),
            "LastModified": u.user_last_modified_date().map(|d| d.to_string()),
        });
        Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        })
    }
}

fn attributes(attrs: &[AttributeType]) -> Value {
    Value::Object(
        attrs
            .iter()
            .map(|a| (a.name().to_owned(), json!(a.value())))
            .collect(),
    )
}

async fn list_users(
    client: &IdpClient,
    pool: &str,
    token: Option<String>,
) -> Result<Page, AwsError> {
    let out = client
        .list_users()
        .user_pool_id(pool)
        .limit(PAGE)
        .set_pagination_token(token)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.users().iter().map(user_row).collect(),
        next: out.pagination_token,
    })
}

fn user_row(u: &UserType) -> Row {
    let attr = |name: &str| {
        u.attributes()
            .iter()
            .find(|a| a.name() == name)
            .and_then(|a| a.value())
    };
    let status = u.user_status().map(|s| s.as_str()).unwrap_or("-");
    let tone = match u.user_status() {
        Some(UserStatusType::Confirmed) => Tone::Good,
        Some(UserStatusType::Compromised) => Tone::Bad,
        _ => Tone::Warn,
    };
    let enabled = if u.enabled() {
        Cell::new("yes")
    } else {
        Cell::toned("no", Tone::Bad)
    };
    Row::new(
        u.username().unwrap_or_default(),
        vec![
            or_dash(u.username()).into(),
            or_dash(attr("email")).into(),
            Cell::toned(status, tone),
            enabled,
            age_of(u.user_create_date().map(|d| d.secs())).into(),
        ],
    )
}

pub struct IdentityPools;

const IDENTITY_COLUMNS: &[Column] = &[Column::fill("NAME", 2), Column::fill("ID", 3)];

#[async_trait]
impl ResourceKind for IdentityPools {
    kind_info! {
        id: "cognito:identitypool",
        title: "Cognito Identity Pools",
        aliases: ["idpool", "identitypools", "ip"],
        scope: Scope::Regional,
        columns: IDENTITY_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let out = IdentityClient::new(ctx.config())
            .list_identity_pools()
            .max_results(PAGE)
            .set_next_token(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let rows = out
            .identity_pools()
            .iter()
            .map(|p| {
                let id = p.identity_pool_id().unwrap_or_default();
                Row::new(id, vec![or_dash(p.identity_pool_name()).into(), id.into()])
            })
            .collect();
        Ok(Page {
            rows,
            next: out.next_token,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let client = IdentityClient::new(ctx.config());
        let (pool, roles) = tokio::join!(
            client.describe_identity_pool().identity_pool_id(id).send(),
            client.get_identity_pool_roles().identity_pool_id(id).send(),
        );
        let p = pool.map_err(AwsError::from_sdk)?;
        let roles = match roles {
            Ok(r) => json!(r.roles()),
            Err(e) => json!(format!("⚠ {}", AwsError::from_sdk(e))),
        };
        let fields = json!({
            "Id": p.identity_pool_id(),
            "Name": p.identity_pool_name(),
            "AllowUnauthenticated": p.allow_unauthenticated_identities(),
            "AllowClassicFlow": p.allow_classic_flow(),
            "UserPoolProviders": p.cognito_identity_providers().iter().map(|c| json!({
                "ProviderName": c.provider_name(),
                "ClientId": c.client_id(),
            })).collect::<Vec<_>>(),
            "LoginProviders": p.supported_login_providers(),
            "OpenIdConnectProviders": p.open_id_connect_provider_arns(),
            "SamlProviders": p.saml_provider_arns(),
            "Roles": roles,
            "Tags": p.identity_pool_tags(),
        });
        Ok(Detail {
            fields,
            raw: format!("{p:#?}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cognitoidentityprovider::operation::list_users::ListUsersOutput;
    use aws_smithy_mocks::{mock, mock_client};

    #[tokio::test]
    async fn users_show_email_and_status_with_the_pagination_token() {
        let rule = mock!(IdpClient::list_users)
            .match_requests(|r| {
                r.user_pool_id() == Some("eu-west-1_abc")
                    && r.limit() == Some(PAGE)
                    && r.pagination_token() == Some("p1")
            })
            .then_output(|| {
                ListUsersOutput::builder()
                    .users(
                        UserType::builder()
                            .username("7f1c-uuid")
                            .attributes(
                                AttributeType::builder()
                                    .name("email")
                                    .value("alice@example.com")
                                    .build()
                                    .unwrap(),
                            )
                            .user_status(UserStatusType::ForceChangePassword)
                            .enabled(true)
                            .build(),
                    )
                    .pagination_token("p2")
                    .build()
            });
        let client = mock_client!(aws_sdk_cognitoidentityprovider, [&rule]);
        let page = list_users(&client, "eu-west-1_abc", Some("p1".into()))
            .await
            .unwrap();
        assert_eq!(page.next.as_deref(), Some("p2"));
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            cells[..4],
            [
                "7f1c-uuid",
                "alice@example.com",
                "FORCE_CHANGE_PASSWORD",
                "yes"
            ]
        );
        assert_eq!(page.rows[0].cells[2].tone, Tone::Warn);
    }

    #[test]
    fn pools_drill_into_their_users() {
        let pool = UserPoolDescriptionType::builder()
            .id("eu-west-1_abc")
            .name("customers")
            .build();
        let drill = pool_row(&pool).drill.unwrap();
        assert_eq!(drill.kind, "cognito:user");
        assert_eq!(drill.parent.id, "eu-west-1_abc");
        assert_eq!(drill.parent.label, "customers");
    }
}
