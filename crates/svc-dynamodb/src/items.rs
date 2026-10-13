//! `dynamodb:item`: a table's items for one partition key, from `Query`.
//! Asked with `Q` on a table.
//!
//! Query only: a Scan reads the whole table and consumes its read capacity,
//! so it isn't offered. Items are read 50 per call; lists pause after a few
//! pages like every other.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{
    AttributeValue, KeyType, ReturnConsumedCapacity, ScalarAttributeType,
};
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ErrorKind, ListQuery, Page, Parent, Query, ResourceKind, Row,
    Scope, kind_info,
};
use serde_json::{Map, Value, json};

pub const PROMPT: &str = "partition key value [= | < | <= | > | >= | begins_with | between] [sort key value [and value]]";

const PAGE: i32 = 50;
/// Separates the parts of an item row's ID.
const SEP: char = '\u{1f}';

pub struct Items;

const COLUMNS: &[Column] = &[Column::fill("KEY", 2), Column::fill("ATTRIBUTES", 5)];

#[async_trait]
impl ResourceKind for Items {
    kind_info! {
        id: "dynamodb:item",
        title: "DynamoDB Items",
        aliases: ["items"],
        scope: Scope::Regional,
        columns: COLUMNS,
        palette: false,
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError> {
        let parent = query
            .parent
            .as_ref()
            .ok_or_else(|| AwsError::not_found("table"))?;
        let (table, input) = Query::split(&parent.id);
        let client = Client::new(ctx.config());
        let schema = key_schema(&client, table).await?;
        query_items(&client, table, &schema, input, query.next.as_deref()).await
    }

    /// The row holds the item; no request.
    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let fields = id
            .split_once(SEP)
            .and_then(|(_, json)| serde_json::from_str(json).ok())
            .unwrap_or(Value::Null);
        Ok(Detail {
            raw: format!("{fields:#}"),
            fields,
        })
    }
}

/// A key attribute: its name and DynamoDB type (`S`, `N` or `B`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyAttr {
    pub name: String,
    pub kind: ScalarAttributeType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Schema {
    pub partition: KeyAttr,
    pub sort: Option<KeyAttr>,
}

async fn key_schema(client: &Client, table: &str) -> Result<Schema, AwsError> {
    let out = client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let t = out.table().ok_or_else(|| AwsError::not_found(table))?;
    let attr = |kind: KeyType| -> Option<KeyAttr> {
        let name = t
            .key_schema()
            .iter()
            .find(|k| *k.key_type() == kind)?
            .attribute_name()
            .to_owned();
        let kind = t
            .attribute_definitions()
            .iter()
            .find(|a| a.attribute_name() == name)
            .map(|a| a.attribute_type().clone())?;
        Some(KeyAttr { name, kind })
    };
    let partition = attr(KeyType::Hash).ok_or_else(|| AwsError::not_found("partition key"))?;
    Ok(Schema {
        partition,
        sort: attr(KeyType::Range),
    })
}

/// What was asked: a partition key value and an optional sort key condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    pub partition: String,
    /// The comparison and its one or two values.
    pub sort: Option<(String, Vec<String>)>,
}

const OPERATORS: [&str; 7] = ["=", "<", "<=", ">", ">=", "begins_with", "between"];

pub(crate) fn parse(input: &str) -> Result<Ask, String> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let Some(at) = words.iter().position(|w| OPERATORS.contains(w)) else {
        return Ok(Ask {
            partition: words.join(" "),
            sort: None,
        });
    };
    let partition = words[..at].join(" ");
    let op = words[at].to_owned();
    let rest = &words[at + 1..];
    let values = if op == "between" {
        let and = rest
            .iter()
            .position(|w| *w == "and")
            .ok_or("between needs: <low> and <high>")?;
        vec![rest[..and].join(" "), rest[and + 1..].join(" ")]
    } else {
        vec![rest.join(" ")]
    };
    if partition.is_empty() || values.iter().any(String::is_empty) {
        return Err(format!("expected: {PROMPT}"));
    }
    Ok(Ask {
        partition,
        sort: Some((op, values)),
    })
}

fn value(attr: &KeyAttr, text: &str) -> Result<AttributeValue, AwsError> {
    match attr.kind {
        ScalarAttributeType::S => Ok(AttributeValue::S(text.to_owned())),
        ScalarAttributeType::N if text.parse::<f64>().is_ok() => {
            Ok(AttributeValue::N(text.to_owned()))
        }
        ScalarAttributeType::N => Err(invalid(format!("{} is a number; got {text}", attr.name))),
        _ => Err(invalid(format!(
            "{} is binary; querying binary keys isn't supported",
            attr.name
        ))),
    }
}

fn invalid(message: String) -> AwsError {
    AwsError::new(ErrorKind::Other, None, message)
}

async fn query_items(
    client: &Client,
    table: &str,
    schema: &Schema,
    input: &str,
    next: Option<&str>,
) -> Result<Page, AwsError> {
    let ask = parse(input).map_err(invalid)?;
    let mut condition = "#pk = :pk".to_owned();
    let mut names = HashMap::from([("#pk".to_owned(), schema.partition.name.clone())]);
    let mut values = HashMap::from([(":pk".to_owned(), value(&schema.partition, &ask.partition)?)]);
    if let Some((op, operands)) = &ask.sort {
        let sort = schema
            .sort
            .as_ref()
            .ok_or_else(|| invalid(format!("{table} has no sort key")))?;
        names.insert("#sk".into(), sort.name.clone());
        values.insert(":a".into(), value(sort, &operands[0])?);
        condition += &match op.as_str() {
            "begins_with" => " AND begins_with(#sk, :a)".to_owned(),
            "between" => {
                values.insert(":b".into(), value(sort, &operands[1])?);
                " AND #sk BETWEEN :a AND :b".to_owned()
            }
            op => format!(" AND #sk {op} :a"),
        };
    }
    let start = next.map(decode_key).transpose()?;
    let out = client
        .query()
        .table_name(table)
        .key_condition_expression(condition)
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .set_exclusive_start_key(start)
        .limit(PAGE)
        .return_consumed_capacity(ReturnConsumedCapacity::Total)
        .send()
        .await
        .map_err(AwsError::from_sdk)?;
    let rows = out.items().iter().map(|item| row(schema, item)).collect();
    let next = out.last_evaluated_key().map(encode_key);
    Ok(Page { rows, next })
}

/// An attribute as plain JSON. Numbers stay strings so no precision is lost;
/// sets become lists and binary values are summarised.
pub(crate) fn to_json(v: &AttributeValue) -> Value {
    match v {
        AttributeValue::S(s) => json!(s),
        AttributeValue::N(n) => json!(n),
        AttributeValue::Bool(b) => json!(b),
        AttributeValue::Null(_) => Value::Null,
        AttributeValue::Ss(items) | AttributeValue::Ns(items) => json!(items),
        AttributeValue::L(items) => Value::Array(items.iter().map(to_json).collect()),
        AttributeValue::M(map) => object(map),
        AttributeValue::B(b) => json!(format!("<{} bytes>", b.as_ref().len())),
        AttributeValue::Bs(items) => json!(format!("<{} binary values>", items.len())),
        _ => json!("<unknown type>"),
    }
}

fn object(map: &HashMap<String, AttributeValue>) -> Value {
    // Sorted, so items read the same way every time.
    let sorted: BTreeMap<&String, &AttributeValue> = map.iter().collect();
    Value::Object(
        sorted
            .into_iter()
            .map(|(k, v)| (k.clone(), to_json(v)))
            .collect::<Map<_, _>>(),
    )
}

fn row(schema: &Schema, item: &HashMap<String, AttributeValue>) -> Row {
    let text = |name: &str| match item.get(name).map(to_json) {
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let mut key = text(&schema.partition.name);
    if let Some(sort) = &schema.sort {
        key = format!("{key} / {}", text(&sort.name));
    }
    let keys: Vec<&str> = std::iter::once(schema.partition.name.as_str())
        .chain(schema.sort.as_ref().map(|s| s.name.as_str()))
        .collect();
    let others: Vec<String> = item
        .keys()
        .filter(|k| !keys.contains(&k.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|k| format!("{k}={}", text(k)))
        .collect();
    let json = object(item);
    Row::new(
        format!("{key}{SEP}{json}"),
        vec![key.into(), others.join("  ").into()],
    )
}

/// `LastEvaluatedKey` as a page token: typed JSON, `{"pk":{"S":"x"}}`.
fn encode_key(key: &HashMap<String, AttributeValue>) -> String {
    let typed: Map<String, Value> = key
        .iter()
        .map(|(k, v)| {
            let typed = match v {
                AttributeValue::S(s) => json!({ "S": s }),
                AttributeValue::N(n) => json!({ "N": n }),
                _ => json!({ "S": "" }),
            };
            (k.clone(), typed)
        })
        .collect();
    Value::Object(typed).to_string()
}

fn decode_key(token: &str) -> Result<HashMap<String, AttributeValue>, AwsError> {
    let map: Map<String, Value> =
        serde_json::from_str(token).map_err(|e| invalid(e.to_string()))?;
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| {
            let value = match (v.get("S"), v.get("N")) {
                (Some(Value::String(s)), _) => AttributeValue::S(s.clone()),
                (_, Some(Value::String(n))) => AttributeValue::N(n.clone()),
                _ => return None,
            };
            Some((k, value))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::operation::query::QueryOutput;
    use aws_smithy_mocks::{mock, mock_client};

    fn schema() -> Schema {
        Schema {
            partition: KeyAttr {
                name: "customerId".into(),
                kind: ScalarAttributeType::S,
            },
            sort: Some(KeyAttr {
                name: "orderedAt".into(),
                kind: ScalarAttributeType::N,
            }),
        }
    }

    #[test]
    fn asks_parse() {
        assert_eq!(
            parse("C-1042").unwrap(),
            Ask {
                partition: "C-1042".into(),
                sort: None
            }
        );
        assert_eq!(
            parse("C-1042 begins_with 2026").unwrap().sort,
            Some(("begins_with".into(), vec!["2026".into()]))
        );
        assert_eq!(
            parse("C-1042 between 1 and 5").unwrap().sort,
            Some(("between".into(), vec!["1".into(), "5".into()]))
        );
        assert!(parse("C-1042 between 1").is_err());
        assert!(parse(">= 5").is_err());
    }

    #[test]
    fn values_follow_the_key_type() {
        let s = schema();
        assert!(value(s.sort.as_ref().unwrap(), "12").is_ok());
        assert!(value(s.sort.as_ref().unwrap(), "twelve").is_err());
    }

    #[test]
    fn page_tokens_round_trip() {
        let key = HashMap::from([
            ("customerId".to_owned(), AttributeValue::S("C-1".into())),
            ("orderedAt".to_owned(), AttributeValue::N("1700".into())),
        ]);
        assert_eq!(decode_key(&encode_key(&key)).unwrap(), key);
    }

    #[tokio::test]
    async fn queries_by_key_and_shows_items_as_plain_json() {
        let rule = mock!(Client::query)
            .match_requests(|r| {
                r.key_condition_expression() == Some("#pk = :pk AND #sk >= :a")
                    && r.expression_attribute_values().is_some_and(|v| {
                        v.get(":pk") == Some(&AttributeValue::S("C-1".into()))
                            && v.get(":a") == Some(&AttributeValue::N("1700".into()))
                    })
                    && r.limit() == Some(PAGE)
            })
            .then_output(|| {
                QueryOutput::builder()
                    .items(HashMap::from([
                        ("customerId".to_owned(), AttributeValue::S("C-1".into())),
                        ("orderedAt".to_owned(), AttributeValue::N("1800".into())),
                        ("status".to_owned(), AttributeValue::S("SHIPPED".into())),
                        ("tags".to_owned(), AttributeValue::Ss(vec!["gift".into()])),
                    ]))
                    .last_evaluated_key("customerId", AttributeValue::S("C-1".into()))
                    .last_evaluated_key("orderedAt", AttributeValue::N("1800".into()))
                    .build()
            });
        let client = mock_client!(aws_sdk_dynamodb, [&rule]);
        let page = query_items(&client, "orders", &schema(), "C-1 >= 1700", None)
            .await
            .unwrap();
        let cells: Vec<_> = page.rows[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(cells, ["C-1 / 1800", "status=SHIPPED  tags=[\"gift\"]"]);
        assert!(page.next.as_deref().unwrap().contains("\"N\":\"1800\""));
        let item: Value = serde_json::from_str(page.rows[0].id.split_once(SEP).unwrap().1).unwrap();
        assert_eq!(item["orderedAt"], "1800");
    }

    #[tokio::test]
    async fn a_sort_condition_on_a_table_without_one_is_explained() {
        let client = mock_client!(aws_sdk_dynamodb, []);
        let mut s = schema();
        s.sort = None;
        let err = query_items(&client, "t", &s, "C-1 = 5", None)
            .await
            .unwrap_err();
        assert_eq!(err.message, "t has no sort key");
    }
}
