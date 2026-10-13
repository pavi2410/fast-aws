//! SES (v2 API): the account's sending status, and email identities.

use async_trait::async_trait;
use aws_sdk_sesv2::Client;
use aws_sdk_sesv2::operation::get_account::GetAccountOutput;
use aws_sdk_sesv2::types::{IdentityInfo, IdentityType, VerificationStatus};
use fast_aws_core::fmt::or_dash;
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::json;

/// Registers every SES resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Account);
    registry.register(Identities);
}

/// The account's SES status in the current region, as a one-row list: sandbox
/// is the first thing to check when mail isn't arriving.
pub struct Account;

const ACCOUNT_COLUMNS: &[Column] = &[
    Column::fixed("ACCESS", 10),
    Column::fixed("SENDING", 8),
    Column::fixed("ENFORCEMENT", 12),
    Column::fill("QUOTA (24H)", 2),
    Column::fixed("MAX RATE", 9),
];

#[async_trait]
impl ResourceKind for Account {
    kind_info! {
        id: "ses:account",
        title: "SES Account",
        aliases: ["ses", "sesaccount"],
        scope: Scope::Regional,
        columns: ACCOUNT_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
        let out = Client::new(ctx.config())
            .get_account()
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        Ok(Page {
            rows: vec![account_row(&out)],
            next: None,
        })
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, _: &str) -> Result<Detail, AwsError> {
        let a = Client::new(ctx.config())
            .get_account()
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let quota = a.send_quota();
        let fields = json!({
            "ProductionAccess": a.production_access_enabled(),
            "SendingEnabled": a.sending_enabled(),
            "EnforcementStatus": a.enforcement_status(),
            "SendQuota": quota.map(|q| json!({
                "Max24HourSend": q.max24_hour_send(),
                "SentLast24Hours": q.sent_last24_hours(),
                "MaxSendRate": q.max_send_rate(),
            })),
            "SuppressedReasons": a.suppression_attributes()
                .map(|s| s.suppressed_reasons().iter().map(|r| r.as_str()).collect::<Vec<_>>()),
            "Details": a.details().map(|d| json!({
                "MailType": d.mail_type().map(|m| m.as_str()),
                "WebsiteUrl": d.website_url(),
                "ReviewStatus": d.review_details().and_then(|r| r.status()).map(|s| s.as_str()),
            })),
        });
        Ok(Detail {
            fields,
            raw: format!("{a:#?}"),
        })
    }
}

fn account_row(a: &GetAccountOutput) -> Row {
    let access = if a.production_access_enabled() {
        Cell::toned("production", Tone::Good)
    } else {
        Cell::toned("⚠ sandbox", Tone::Warn)
    };
    let sending = if a.sending_enabled() {
        Cell::toned("on", Tone::Good)
    } else {
        Cell::toned("paused", Tone::Bad)
    };
    let enforcement = a.enforcement_status().unwrap_or("-");
    let enforcement_tone = if enforcement == "HEALTHY" {
        Tone::Good
    } else {
        Tone::Bad
    };
    let (quota, rate) = match a.send_quota() {
        Some(q) => (
            format!("{} / {}", q.sent_last24_hours(), q.max24_hour_send()),
            format!("{}/s", q.max_send_rate()),
        ),
        None => ("-".into(), "-".into()),
    };
    Row::new(
        "account",
        vec![
            access,
            sending,
            Cell::toned(enforcement, enforcement_tone),
            quota.into(),
            rate.into(),
        ],
    )
}

pub struct Identities;

const IDENTITY_COLUMNS: &[Column] = &[
    Column::fill("IDENTITY", 3),
    Column::fixed("TYPE", 14),
    Column::fixed("VERIFICATION", 16),
    Column::fixed("SENDING", 8),
];

#[async_trait]
impl ResourceKind for Identities {
    kind_info! {
        id: "ses:identity",
        title: "SES Identities",
        aliases: ["identity", "identities", "sesid"],
        scope: Scope::Regional,
        columns: IDENTITY_COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    /// Includes the DNS records a domain identity needs, ready to copy.
    async fn describe(
        &self,
        ctx: &Ctx,
        _: Option<&Parent>,
        name: &str,
    ) -> Result<Detail, AwsError> {
        let i = Client::new(ctx.config())
            .get_email_identity()
            .email_identity(name)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let dkim = i.dkim_attributes();
        let is_domain = i.identity_type() == Some(&IdentityType::Domain);
        let dkim_records: Vec<String> = dkim
            .filter(|_| is_domain)
            .map(|d| d.tokens())
            .unwrap_or_default()
            .iter()
            .map(|t| format!("{t}._domainkey.{name} CNAME {t}.dkim.amazonses.com"))
            .collect();
        let mail_from = i.mail_from_attributes();
        let fields = json!({
            "Identity": name,
            "Type": i.identity_type().map(|t| t.as_str()),
            "VerificationStatus": i.verification_status().map(|s| s.as_str()),
            "VerifiedForSending": i.verified_for_sending_status(),
            "Dkim": dkim.map(|d| json!({
                "Status": d.status().map(|s| s.as_str()),
                "SigningEnabled": d.signing_enabled(),
                "Origin": d.signing_attributes_origin().map(|o| o.as_str()),
            })),
            "DnsRecords": dkim_records,
            "MailFrom": mail_from.map(|m| json!({
                "Domain": m.mail_from_domain(),
                "Status": m.mail_from_domain_status().as_str(),
                "OnMxFailure": m.behavior_on_mx_failure().as_str(),
            })),
            "ConfigurationSet": i.configuration_set_name(),
            "FeedbackForwarding": i.feedback_forwarding_status(),
            "PolicyNames": i.policies().map(|p| p.keys().collect::<Vec<_>>()),
        });
        Ok(Detail {
            fields,
            raw: format!("{i:#?}"),
        })
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .list_email_identities()
        .page_size(1000)
        .set_next_token(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    Ok(Page {
        rows: out.email_identities().iter().map(identity_row).collect(),
        next: out.next_token,
    })
}

fn identity_row(i: &IdentityInfo) -> Row {
    let status = i.verification_status().map(|s| s.as_str()).unwrap_or("-");
    let tone = match i.verification_status() {
        Some(VerificationStatus::Success) => Tone::Good,
        Some(VerificationStatus::Failed) => Tone::Bad,
        _ => Tone::Warn,
    };
    let sending = if i.sending_enabled() {
        Cell::new("on")
    } else {
        Cell::toned("off", Tone::Dim)
    };
    Row::new(
        i.identity_name().unwrap_or_default(),
        vec![
            or_dash(i.identity_name()).into(),
            or_dash(i.identity_type().map(|t| t.as_str())).into(),
            Cell::toned(status, tone),
            sending,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_sesv2::operation::list_email_identities::ListEmailIdentitiesOutput;
    use aws_sdk_sesv2::types::SendQuota;
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn the_sandbox_stands_out() {
        let account = GetAccountOutput::builder()
            .production_access_enabled(false)
            .sending_enabled(true)
            .enforcement_status("HEALTHY")
            .send_quota(
                SendQuota::builder()
                    .max24_hour_send(200.0)
                    .sent_last24_hours(12.0)
                    .max_send_rate(1.0)
                    .build(),
            )
            .build();
        let row = account_row(&account);
        assert_eq!(row.cells[0].text, "⚠ sandbox");
        assert_eq!(row.cells[0].tone, Tone::Warn);
        assert_eq!(row.cells[3].text, "12 / 200");
        assert_eq!(row.cells[4].text, "1/s");
    }

    #[tokio::test]
    async fn identities_show_verification() {
        let rule = mock!(Client::list_email_identities)
            .match_requests(|r| r.next_token() == Some("t1"))
            .then_output(|| {
                ListEmailIdentitiesOutput::builder()
                    .email_identities(
                        IdentityInfo::builder()
                            .identity_name("example.com")
                            .identity_type(IdentityType::Domain)
                            .verification_status(VerificationStatus::Pending)
                            .sending_enabled(true)
                            .build(),
                    )
                    .next_token("t2")
                    .build()
            });
        let client = mock_client!(aws_sdk_sesv2, [&rule]);
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
        assert_eq!(page.rows[0].cells[2].text, "PENDING");
        assert_eq!(page.rows[0].cells[2].tone, Tone::Warn);
    }
}
