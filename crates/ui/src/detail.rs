//! State of the detail (`d`) view, and the YAML-style layout of its fields.

use fast_aws_core::{AwsError, Detail};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum DetailState {
    Loading,
    Ready(Detail),
    Failed(AwsError),
}

pub struct DetailView {
    /// The breadcrumb of the list it was opened from.
    pub title: String,
    pub id: String,
    pub generation: u64,
    pub state: DetailState,
    /// Show the SDK `Debug` output instead of the curated fields (`Tab`).
    pub raw: bool,
    pub scroll: u16,
}

impl DetailView {
    pub fn new(title: impl Into<String>, id: String, generation: u64) -> Self {
        Self {
            title: title.into(),
            id,
            generation,
            state: DetailState::Loading,
            raw: false,
            scroll: 0,
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll = (i32::from(self.scroll) + delta).clamp(0, i32::from(u16::MAX)) as u16;
    }
}

/// One line of the YAML-style layout. Keys and values are kept apart so the
/// renderer can colour them differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailLine {
    pub indent: usize,
    /// `- ` prefix for list items.
    pub bullet: bool,
    pub key: Option<String>,
    pub value: Option<String>,
}

impl DetailLine {
    fn new(indent: usize, bullet: bool, key: Option<&str>, value: Option<String>) -> Self {
        Self {
            indent,
            bullet,
            key: key.map(str::to_owned),
            value,
        }
    }
}

impl std::fmt::Display for DetailLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", "  ".repeat(self.indent))?;
        if self.bullet {
            f.write_str(if self.key.is_none() && self.value.is_none() {
                "-"
            } else {
                "- "
            })?;
        }
        match (&self.key, &self.value) {
            (Some(k), Some(v)) => write!(f, "{k}: {v}"),
            (Some(k), None) => write!(f, "{k}:"),
            (None, Some(v)) => f.write_str(v),
            (None, None) => Ok(()),
        }
    }
}

/// Lays out a JSON value as YAML-style lines. Null fields are left out.
pub fn layout(value: &Value) -> Vec<DetailLine> {
    let mut lines = Vec::new();
    push_value(&mut lines, 0, false, None, value);
    lines
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) if a.is_empty() => Some("[]".into()),
        Value::Object(o) if o.is_empty() => Some("{}".into()),
        _ => None,
    }
}

fn push_value(
    lines: &mut Vec<DetailLine>,
    indent: usize,
    bullet: bool,
    key: Option<&str>,
    value: &Value,
) {
    if value.is_null() {
        return;
    }
    if let Some(s) = scalar(value) {
        lines.push(DetailLine::new(indent, bullet, key, Some(s)));
        return;
    }
    // A container: put its key (or a bare bullet) on its own line, then the children.
    // A key after a bullet sits one level further right, so its children do too.
    let opens_line = key.is_some() || bullet;
    let child_indent = indent + usize::from(opens_line) + usize::from(key.is_some() && bullet);
    if opens_line {
        lines.push(DetailLine::new(indent, bullet, key, None));
    }
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                push_value(lines, child_indent, false, Some(k), v);
            }
        }
        Value::Array(items) => {
            for item in items {
                push_item(lines, child_indent, item);
            }
        }
        _ => unreachable!("scalars handled above"),
    }
}

/// A list item. An object's first field shares the bullet line (`- GroupId: sg-1`)
/// and the rest line up under it, as in YAML.
fn push_item(lines: &mut Vec<DetailLine>, indent: usize, item: &Value) {
    let Value::Object(map) = item else {
        return push_value(lines, indent, true, None, item);
    };
    let mut fields = map.iter().filter(|(_, v)| !v.is_null());
    let Some((first_key, first)) = fields.next() else {
        return push_value(
            lines,
            indent,
            true,
            None,
            &Value::Object(Default::default()),
        );
    };
    push_value(lines, indent, true, Some(first_key), first);
    for (k, v) in fields {
        push_value(lines, indent + 1, false, Some(k), v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(v: &Value) -> String {
        layout(v).iter().map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn nested_values_lay_out_like_yaml() {
        let v = json!({
            "InstanceId": "i-1",
            "Missing": null,
            "Count": 2,
            "SecurityGroups": [{"GroupId": "sg-1", "GroupName": "web"}],
            "Tags": {"Name": "web-1", "env": "prod"},
            "Empty": [],
        });
        assert_eq!(
            text(&v),
            "InstanceId: i-1\n\
             Count: 2\n\
             SecurityGroups:\n\
             \x20 - GroupId: sg-1\n\
             \x20   GroupName: web\n\
             Tags:\n\
             \x20 Name: web-1\n\
             \x20 env: prod\n\
             Empty: []\n"
        );
    }

    #[test]
    fn nested_containers_inside_list_items_indent_past_the_bullet() {
        let v = json!({"Rules": [{"Ports": [80, 443], "Cidr": "0.0.0.0/0"}]});
        assert_eq!(
            text(&v),
            "Rules:\n  - Ports:\n      - 80\n      - 443\n    Cidr: 0.0.0.0/0\n"
        );
    }

    #[test]
    fn scalar_list_items_are_bulleted() {
        assert_eq!(text(&json!({"Ids": ["a", "b"]})), "Ids:\n  - a\n  - b\n");
    }

    #[test]
    fn scroll_never_goes_negative() {
        let mut d = DetailView::new("Things", "a".into(), 1);
        d.scroll_by(-3);
        assert_eq!(d.scroll, 0);
        d.scroll_by(5);
        assert_eq!(d.scroll, 5);
    }
}
