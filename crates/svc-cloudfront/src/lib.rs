//! CloudFront: distributions and their invalidations.
//!
//! CloudFront is global; the runtime sends these calls to `us-east-1`.

use async_trait::async_trait;
use aws_sdk_cloudfront::Client;
use aws_sdk_cloudfront::types::{
    CacheBehavior, DefaultCacheBehavior, DistributionSummary, InvalidationSummary, Origin,
};
use fast_aws_core::fmt::age_of;
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, ResourceKind, Row, Scope, Tone,
    kind_info,
};
use serde_json::{Value, json};

/// Registers every CloudFront resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Distributions);
    registry.register(Invalidations);
}

pub struct Distributions;

const COLUMNS: &[Column] = &[
    Column::fixed("ID", 14),
    Column::fill("ALIASES", 3),
    Column::fill("DOMAIN", 2),
    Column::fixed("STATUS", 10),
    Column::fixed("ENABLED", 7),
    Column::fixed("ORIGINS", 7),
    Column::fill("COMMENT", 2),
];

#[async_trait]
impl ResourceKind for Distributions {
    kind_info! {
        id: "cloudfront:distribution",
        title: "CloudFront Distributions",
        aliases: ["cf", "cloudfront", "dist", "distributions"],
        scope: Scope::Global,
        columns: COLUMNS,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        list(&Client::new(ctx.config()), query).await
    }

    async fn describe(&self, ctx: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let out = Client::new(ctx.config())
            .get_distribution()
            .id(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let d = out.distribution().ok_or_else(|| AwsError::not_found(id))?;
        let c = d.distribution_config();
        let cert = c.and_then(|c| c.viewer_certificate());
        let mut behaviors: Vec<Value> = Vec::new();
        if let Some(default) = c.and_then(|c| c.default_cache_behavior()) {
            behaviors.push(default_behavior(default));
        }
        behaviors.extend(
            c.and_then(|c| c.cache_behaviors())
                .map(|b| b.items())
                .unwrap_or_default()
                .iter()
                .map(behavior),
        );
        let fields = json!({
            "Id": d.id(),
            "Arn": d.arn(),
            "DomainName": d.domain_name(),
            "Status": d.status(),
            "Enabled": c.map(|c| c.enabled()),
            "Comment": c.map(|c| c.comment()),
            "Aliases": c.and_then(|c| c.aliases()).map(|a| a.items()),
            "Origins": c.and_then(|c| c.origins()).map(|o| o.items()).unwrap_or_default().iter().map(origin).collect::<Vec<_>>(),
            // Ordered as CloudFront evaluates them: specific patterns, default last.
            "Behaviors": behaviors,
            "Certificate": cert.map(|v| json!({
                "AcmCertificateArn": v.acm_certificate_arn(),
                "MinimumProtocolVersion": v.minimum_protocol_version().map(|p| p.as_str()),
                "DefaultCertificate": v.cloud_front_default_certificate(),
            })),
            "WebAclId": c.and_then(|c| c.web_acl_id()).filter(|w| !w.is_empty()),
            "PriceClass": c.and_then(|c| c.price_class()).map(|p| p.as_str()),
            "DefaultRootObject": c.and_then(|c| c.default_root_object()),
            "Logging": c.and_then(|c| c.logging()).filter(|l| l.enabled()).map(|l| l.bucket()),
            "InProgressInvalidations": d.in_progress_invalidation_batches(),
            "LastModified": d.last_modified_time().to_string(),
        });
        Ok(Detail {
            fields,
            raw: format!("{d:#?}"),
        })
    }
}

async fn list(client: &Client, query: &ListQuery) -> Result<Page, AwsError> {
    let out = client
        .list_distributions()
        .max_items(100)
        .set_marker(query.next.clone())
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let Some(list) = out.distribution_list() else {
        return Ok(Page::default());
    };
    let next = list
        .next_marker()
        .filter(|_| list.is_truncated())
        .map(str::to_owned);
    Ok(Page {
        rows: list.items().iter().map(|d| origins(row(d), d)).collect(),
        next,
    })
}

fn row(d: &DistributionSummary) -> Row {
    let aliases = d.aliases().map(|a| a.items()).unwrap_or_default();
    let aliases = match aliases {
        [] => "-".to_owned(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    };
    let status = if d.status() == "Deployed" {
        Cell::toned("Deployed", Tone::Good)
    } else {
        Cell::toned(d.status(), Tone::Warn)
    };
    let enabled = if d.enabled() {
        Cell::new("yes")
    } else {
        Cell::toned("no", Tone::Dim)
    };
    let parent = Parent {
        id: d.id().to_owned(),
        label: d.id().to_owned(),
        region: None,
    };
    Row::new(
        d.id(),
        vec![
            d.id().into(),
            aliases.into(),
            d.domain_name().into(),
            status,
            enabled,
            d.origins()
                .map(|o| o.quantity())
                .unwrap_or_default()
                .to_string()
                .into(),
            d.comment().into(),
        ],
    )
    .drill("cloudfront:invalidation", parent)
}

fn origin(o: &Origin) -> Value {
    let kind = if o.s3_origin_config().is_some() || o.domain_name().contains(".s3.") {
        "S3"
    } else if o.vpc_origin_config().is_some() {
        "VPC"
    } else {
        "custom"
    };
    json!({
        "Id": o.id(),
        "Domain": o.domain_name(),
        "Path": o.origin_path().filter(|p| !p.is_empty()),
        "Type": kind,
        "OriginAccessControl": o.origin_access_control_id().filter(|p| !p.is_empty()),
    })
}

/// One behaviour: which path goes where, and how it's cached.
fn behavior_fields(
    path: &str,
    target: &str,
    protocol: &str,
    cache_policy: Option<&str>,
    functions: usize,
    edge_lambdas: usize,
) -> Value {
    json!({
        "PathPattern": path,
        "Origin": target,
        "ViewerProtocol": protocol,
        "CachePolicyId": cache_policy,
        "Functions": functions,
        "LambdaAtEdge": edge_lambdas,
    })
}

fn default_behavior(b: &DefaultCacheBehavior) -> Value {
    behavior_fields(
        "* (default)",
        b.target_origin_id(),
        b.viewer_protocol_policy().as_str(),
        b.cache_policy_id(),
        b.function_associations()
            .map(|f| f.quantity())
            .unwrap_or_default() as usize,
        b.lambda_function_associations()
            .map(|l| l.quantity())
            .unwrap_or_default() as usize,
    )
}

fn behavior(b: &CacheBehavior) -> Value {
    behavior_fields(
        b.path_pattern(),
        b.target_origin_id(),
        b.viewer_protocol_policy().as_str(),
        b.cache_policy_id(),
        b.function_associations()
            .map(|f| f.quantity())
            .unwrap_or_default() as usize,
        b.lambda_function_associations()
            .map(|l| l.quantity())
            .unwrap_or_default() as usize,
    )
}

pub struct Invalidations;

const INVALIDATION_COLUMNS: &[Column] = &[
    Column::fill("ID", 2),
    Column::fixed("STATUS", 11),
    Column::fixed("AGE", 5),
];

#[async_trait]
impl ResourceKind for Invalidations {
    kind_info! {
        id: "cloudfront:invalidation",
        title: "Invalidations",
        aliases: ["invalidations"],
        scope: Scope::Global,
        columns: INVALIDATION_COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let dist = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("distribution"))?;
        let out = Client::new(ctx.config())
            .list_invalidations()
            .distribution_id(&dist.id)
            .max_items(100)
            .set_marker(query.next.clone())
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let Some(list) = out.invalidation_list() else {
            return Ok(Page::default());
        };
        let next = list
            .next_marker()
            .filter(|_| list.is_truncated())
            .map(str::to_owned);
        Ok(Page {
            rows: list.items().iter().map(invalidation_row).collect(),
            next,
        })
    }

    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        id: &str,
    ) -> Result<Detail, AwsError> {
        let dist = parent.ok_or_else(|| AwsError::not_found("distribution"))?;
        let out = Client::new(ctx.config())
            .get_invalidation()
            .distribution_id(&dist.id)
            .id(id)
            .send()
            .await
            .map_err(AwsError::from_sdk)?;
        let i = out.invalidation().ok_or_else(|| AwsError::not_found(id))?;
        let fields = json!({
            "Id": i.id(),
            "Status": i.status(),
            "CreateTime": i.create_time().to_string(),
            "Paths": i.invalidation_batch().and_then(|b| b.paths()).map(|p| p.items()),
            "CallerReference": i.invalidation_batch().map(|b| b.caller_reference()),
        });
        Ok(Detail {
            fields,
            raw: format!("{i:#?}"),
        })
    }
}

fn invalidation_row(i: &InvalidationSummary) -> Row {
    let tone = if i.status() == "Completed" {
        Tone::Dim
    } else {
        Tone::Warn
    };
    Row::new(
        i.id(),
        vec![
            i.id().into(),
            Cell::toned(i.status(), tone),
            age_of(Some(i.create_time().secs())).into(),
        ],
    )
}

/// What `g` offers from a distribution: the S3 buckets it serves from.
fn origins(mut row: Row, d: &DistributionSummary) -> Row {
    for o in d.origins().map(|o| o.items()).unwrap_or_default() {
        // `<bucket>.s3.amazonaws.com` or `<bucket>.s3.<region>.amazonaws.com`.
        if let Some((bucket, _)) = o.domain_name().split_once(".s3.") {
            row = row.relate(format!("bucket {bucket}"), "s3:bucket", bucket);
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudfront::operation::list_distributions::ListDistributionsOutput;
    use aws_sdk_cloudfront::primitives::DateTime;
    use aws_sdk_cloudfront::types::{Aliases, DistributionList, HttpVersion, Origins, PriceClass};
    use aws_smithy_mocks::{mock, mock_client};

    fn summary() -> DistributionSummary {
        DistributionSummary::builder()
            .id("E123")
            .arn("arn:aws:cloudfront::1:distribution/E123")
            .status("InProgress")
            .last_modified_time(DateTime::from_secs(0))
            .domain_name("d111.cloudfront.net")
            .aliases(
                Aliases::builder()
                    .quantity(2)
                    .items("a.example.com")
                    .items("b.example.com")
                    .build()
                    .unwrap(),
            )
            .origins(
                Origins::builder()
                    .quantity(1)
                    .items(
                        Origin::builder()
                            .id("site")
                            .domain_name("site.s3.amazonaws.com")
                            .build()
                            .unwrap(),
                    )
                    .build()
                    .unwrap(),
            )
            .comment("site")
            .price_class(PriceClass::PriceClassAll)
            .enabled(true)
            .web_acl_id("")
            .http_version(HttpVersion::Http2)
            .is_ipv6_enabled(true)
            .staging(false)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn distributions_page_only_while_truncated() {
        let rule = mock!(Client::list_distributions)
            .match_requests(|r| r.marker() == Some("m1"))
            .then_output(|| {
                ListDistributionsOutput::builder()
                    .distribution_list(
                        DistributionList::builder()
                            .marker("m1")
                            .next_marker("m2")
                            .is_truncated(false)
                            .max_items(100)
                            .quantity(1)
                            .items(summary())
                            .build()
                            .unwrap(),
                    )
                    .build()
            });
        let client = mock_client!(aws_sdk_cloudfront, [&rule]);
        let page = list(
            &client,
            &ListQuery {
                parent: None,
                next: Some("m1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.next, None);
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            cells,
            [
                "E123",
                "a.example.com +1",
                "d111.cloudfront.net",
                "InProgress",
                "yes",
                "1",
                "site"
            ]
        );
        assert_eq!(page.rows[0].cells[3].tone, Tone::Warn);
        assert_eq!(
            page.rows[0].drill.as_ref().unwrap().kind,
            "cloudfront:invalidation"
        );
        let origin = &page.rows[0].relations[0];
        assert_eq!(
            (origin.kind, origin.filter.as_deref()),
            ("s3:bucket", Some("site"))
        );
    }
}
