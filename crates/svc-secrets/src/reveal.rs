//! `secrets:value`: a secret's current value, shown only after `v` and typing
//! `reveal`. This is the only place `GetSecretValue` is called.
//!
//! Values are never cached (leaving the view drops them), and the describe and
//! raw views show the same rows, nothing more.

use async_trait::async_trait;
use aws_sdk_secretsmanager::Client;
use fast_aws_core::{
    AwsError, Cell, Column, Ctx, Detail, ErrorKind, ListQuery, Page, Parent, Query, ResourceKind,
    Row, Scope, Tone, kind_info,
};
use serde_json::{Value, json};

pub const PROMPT: &str = "type reveal to show this secret's value on screen";
const CONFIRMATION: &str = "reveal";

pub struct Values;

const COLUMNS: &[Column] = &[Column::fill("KEY", 1), Column::fill("VALUE", 3)];

#[async_trait]
impl ResourceKind for Values {
    kind_info! {
        id: "secrets:value",
        title: "Secret Value",
        aliases: ["secretvalue"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    fn cacheable(&self) -> bool {
        false
    }

    fn refresh_on_open(&self) -> bool {
        false
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("secret"))?;
        let (secret, answer) = Query::split(&parent.id);
        if answer.trim() != CONFIRMATION {
            return Err(AwsError::new(
                ErrorKind::Other,
                None,
                "not revealed: type reveal to confirm",
            ));
        }
        reveal(&Client::new(ctx.config()), secret).await
    }

    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let fields = json!({ "Key": id });
        Ok(Detail {
            raw: id.to_owned(),
            fields,
        })
    }
}

async fn reveal(client: &Client, secret: &str) -> Result<Page, AwsError> {
    let out = client
        .get_secret_value()
        .secret_id(secret)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = match (out.secret_string(), out.secret_binary()) {
        (Some(text), _) => rows(text),
        (None, Some(binary)) => vec![Row::new(
            "binary",
            vec![
                "(binary)".into(),
                Cell::toned(
                    format!("{} bytes, not shown", binary.as_ref().len()),
                    Tone::Dim,
                ),
            ],
        )],
        _ => Vec::new(),
    };
    Ok(Page { rows, next: None })
}

/// A JSON object becomes one row per key; anything else is a single row.
fn rows(text: &str) -> Vec<Row> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map
            .into_iter()
            .map(|(k, v)| {
                let shown = match v {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                Row::new(k.clone(), vec![k.into(), shown.into()])
            })
            .collect(),
        _ => vec![Row::new("value", vec!["(value)".into(), text.into()])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueOutput;
    use aws_smithy_mocks::{mock, mock_client};

    #[test]
    fn json_secrets_become_key_rows() {
        let rows = rows(r#"{"username":"app","port":5432}"#);
        let cells: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.text.as_str()).collect())
            .collect();
        assert_eq!(cells, [vec!["username", "app"], vec!["port", "5432"]]);
        assert_eq!(rows_text("plain"), ["(value)", "plain"]);
    }

    fn rows_text(text: &str) -> Vec<String> {
        rows(text)[0].cells.iter().map(|c| c.text.clone()).collect()
    }

    #[tokio::test]
    async fn the_value_is_fetched_only_with_the_confirmation() {
        let rule = mock!(Client::get_secret_value)
            .match_requests(|r| r.secret_id() == Some("arn:s"))
            .then_output(|| {
                GetSecretValueOutput::builder()
                    .secret_string("hunter2")
                    .build()
            });
        let client = mock_client!(aws_sdk_secretsmanager, [&rule]);
        let page = reveal(&client, "arn:s").await.unwrap();
        assert_eq!(page.rows[0].cells[1].text, "hunter2");
        assert_eq!(rule.num_calls(), 1);
        assert!(!Values.cacheable());
    }
}
