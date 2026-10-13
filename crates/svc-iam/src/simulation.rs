//! `iam:simulation`: "can this role do X?", answered by
//! `SimulatePrincipalPolicy` (a read-only call). `S` on a role or user asks for
//! the actions, optionally `on` some resource ARNs.

use async_trait::async_trait;
use aws_sdk_iam::Client;
use aws_sdk_iam::types::{EvaluationResult, PolicyEvaluationDecisionType};
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ListQuery, Page, Parent, Query, ResourceKind, Row, Scope,
    Tone, kind_info,
};
use serde_json::json;

pub const PROMPT: &str = "actions to test (e.g. s3:GetObject, sqs:SendMessage on arn:…)";

pub struct Simulations;

const COLUMNS: &[Column] = &[
    Column::fill("ACTION", 2),
    Column::fill("RESOURCE", 2),
    Column::fixed("DECISION", 13),
    Column::fill("DECIDED BY", 3),
];

#[async_trait]
impl ResourceKind for Simulations {
    kind_info! {
        id: "iam:simulation",
        title: "Policy Simulation",
        aliases: ["simulation"],
        scope: Scope::Global,
        columns: COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("role or user"))?;
        let (principal_arn, input) = Query::split(&parent.id);
        simulate(
            &Client::new(ctx.config()),
            principal_arn,
            input,
            query.next.clone(),
        )
        .await
    }

    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let fields = json!({ "Result": id });
        Ok(Detail {
            raw: id.to_owned(),
            fields,
        })
    }
}

/// `"s3:GetObject, s3:PutObject on arn:a, arn:b"` → actions and resources.
pub(crate) fn parse(input: &str) -> (Vec<String>, Vec<String>) {
    let split = |s: &str| -> Vec<String> {
        s.split([',', ' '])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect()
    };
    match input.split_once(" on ") {
        Some((actions, resources)) => (split(actions), split(resources)),
        None => (split(input), Vec::new()),
    }
}

async fn simulate(
    client: &Client,
    principal_arn: &str,
    input: &str,
    marker: Option<String>,
) -> Result<Page, AwsError> {
    let (actions, resources) = parse(input);
    let out = client
        .simulate_principal_policy()
        .policy_source_arn(principal_arn)
        .set_action_names(Some(actions))
        .set_resource_arns((!resources.is_empty()).then_some(resources))
        .set_marker(marker)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let next = out
        .marker()
        .filter(|_| out.is_truncated())
        .map(str::to_owned);
    Ok(Page {
        rows: out.evaluation_results().iter().map(row).collect(),
        next,
    })
}

fn row(r: &EvaluationResult) -> Row {
    let decision = match r.eval_decision() {
        PolicyEvaluationDecisionType::Allowed => Cell::toned("✓ allowed", Tone::Good),
        PolicyEvaluationDecisionType::ExplicitDeny => Cell::toned("✕ denied", Tone::Bad),
        PolicyEvaluationDecisionType::ImplicitDeny => Cell::toned("✕ not allowed", Tone::Warn),
        other => Cell::new(other.as_str()),
    };
    let decided_by: Vec<&str> = r
        .matched_statements()
        .iter()
        .filter_map(|s| s.source_policy_id())
        .collect();
    let decided_by = if decided_by.is_empty() {
        "no statement matches".to_owned()
    } else {
        decided_by.join(", ")
    };
    let resource = r.eval_resource_name().unwrap_or("*");
    let id = format!(
        "{} on {resource}: {} ({decided_by})",
        r.eval_action_name(),
        r.eval_decision().as_str()
    );
    Row::new(
        id,
        vec![
            r.eval_action_name().into(),
            resource.into(),
            decision,
            decided_by.into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_iam::operation::simulate_principal_policy::SimulatePrincipalPolicyOutput;
    use aws_sdk_iam::types::Statement;
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn input_parses_into_actions_and_resources() {
        assert_eq!(
            parse("s3:GetObject, sqs:SendMessage"),
            (
                vec!["s3:GetObject".into(), "sqs:SendMessage".into()],
                vec![]
            )
        );
        assert_eq!(
            parse("s3:GetObject on arn:aws:s3:::b/*"),
            (vec!["s3:GetObject".into()], vec!["arn:aws:s3:::b/*".into()])
        );
    }

    #[tokio::test]
    async fn results_show_the_decision_and_what_decided_it() {
        let rule = mock!(Client::simulate_principal_policy)
            .match_requests(|r| {
                r.policy_source_arn() == Some("arn:aws:iam::1:role/r")
                    && r.action_names() == ["s3:GetObject".to_owned(), "s3:DeleteObject".to_owned()]
            })
            .then_output(|| {
                SimulatePrincipalPolicyOutput::builder()
                    .evaluation_results(
                        EvaluationResult::builder()
                            .eval_action_name("s3:GetObject")
                            .eval_decision(PolicyEvaluationDecisionType::Allowed)
                            .matched_statements(
                                Statement::builder()
                                    .source_policy_id("ReadOnlyAccess")
                                    .build(),
                            )
                            .build()
                            .unwrap(),
                    )
                    .evaluation_results(
                        EvaluationResult::builder()
                            .eval_action_name("s3:DeleteObject")
                            .eval_decision(PolicyEvaluationDecisionType::ImplicitDeny)
                            .build()
                            .unwrap(),
                    )
                    .is_truncated(false)
                    .build()
            });
        let client = mock_client!(aws_sdk_iam, [&rule]);
        let page = simulate(
            &client,
            "arn:aws:iam::1:role/r",
            "s3:GetObject s3:DeleteObject",
            None,
        )
        .await
        .unwrap();
        let cells = |i: usize| {
            page.rows[i]
                .cells
                .iter()
                .map(|c| c.text.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cells(0),
            ["s3:GetObject", "*", "✓ allowed", "ReadOnlyAccess"]
        );
        assert_eq!(
            cells(1),
            [
                "s3:DeleteObject",
                "*",
                "✕ not allowed",
                "no statement matches"
            ]
        );
        assert_eq!(page.next, None);
    }
}
