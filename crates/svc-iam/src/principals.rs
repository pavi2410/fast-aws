//! Summarising a role's trust policy: who can assume it.

use serde_json::Value;

/// The principals allowed by a trust policy, shortened for a table cell:
/// services without `.amazonaws.com`, accounts as `acct 123456789012`, and
/// federated providers by their last path segment.
pub(crate) fn trusted_by(trust: &Value) -> Vec<String> {
    let statements = match &trust["Statement"] {
        Value::Array(list) => list.iter().collect(),
        single @ Value::Object(_) => vec![single],
        _ => Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for statement in statements.into_iter().filter(|s| s["Effect"] != "Deny") {
        let principal = &statement["Principal"];
        if principal == "*" {
            out.push("anyone (*)".into());
            continue;
        }
        for (kind, values) in principal.as_object().into_iter().flatten() {
            for value in strings(values) {
                let short = match kind.as_str() {
                    "Service" => value.trim_end_matches(".amazonaws.com").to_owned(),
                    "AWS" => account_of(value),
                    "Federated" => value.rsplit('/').next().unwrap_or(value).to_owned(),
                    _ => value.to_owned(),
                };
                if !out.contains(&short) {
                    out.push(short);
                }
            }
        }
    }
    out
}

/// A policy value that may be a string or a list of strings.
fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// `arn:aws:iam::123456789012:root` → `acct 123456789012`; other ARNs keep
/// their resource part (`role/Admin`), bare account IDs get the prefix.
fn account_of(value: &str) -> String {
    let parts: Vec<&str> = value.split(':').collect();
    match parts.as_slice() {
        [_, _, _, _, account, "root"] => format!("acct {account}"),
        [_, _, _, _, account, resource] => format!("{account}:{resource}"),
        [account] if account.chars().all(|c| c.is_ascii_digit()) => format!("acct {account}"),
        _ => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarises_services_accounts_and_federation() {
        let trust = json!({"Statement": [
            {"Effect": "Allow", "Principal": {"Service": ["lambda.amazonaws.com", "edgelambda.amazonaws.com"]}},
            {"Effect": "Allow", "Principal": {"AWS": "arn:aws:iam::111122223333:root"}},
            {"Effect": "Allow", "Principal": {"AWS": ["arn:aws:iam::444455556666:role/Deployer", "777788889999"]}},
            {"Effect": "Allow", "Principal": {"Federated": "arn:aws:iam::1:oidc-provider/token.actions.githubusercontent.com"}},
            {"Effect": "Deny", "Principal": {"Service": "ec2.amazonaws.com"}},
        ]});
        assert_eq!(
            trusted_by(&trust),
            [
                "lambda",
                "edgelambda",
                "acct 111122223333",
                "444455556666:role/Deployer",
                "acct 777788889999",
                "token.actions.githubusercontent.com",
            ]
        );
    }

    #[test]
    fn wildcard_and_single_statement() {
        assert_eq!(
            trusted_by(&json!({"Statement": {"Effect": "Allow", "Principal": "*"}})),
            ["anyone (*)"]
        );
        assert!(trusted_by(&json!(null)).is_empty());
    }
}
