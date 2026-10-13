//! The start screen when no resource type is named on the command line: every
//! resource type, with the names the `:` palette accepts. It makes no AWS
//! calls; Enter opens the selected type.

use async_trait::async_trait;
use fast_aws_core::{
    AwsError, Column, Ctx, Detail, ListQuery, Page, Parent, Registry, ResourceKind, Row, Scope,
    kind_info,
};
use serde_json::json;

pub const HOME: &str = "home";

pub struct Home {
    /// (kind ID, title, aliases) of every palette kind.
    entries: Vec<(&'static str, &'static str, &'static [&'static str])>,
}

impl Home {
    /// The home list for every palette kind registered so far.
    pub fn new(registry: &Registry) -> Self {
        let entries = registry
            .palette()
            .map(|k| (k.id(), k.title(), k.aliases()))
            .collect();
        Self { entries }
    }
}

const COLUMNS: &[Column] = &[Column::fill("RESOURCE", 2), Column::fill("JUMP WITH", 3)];

#[async_trait]
impl ResourceKind for Home {
    kind_info! {
        id: HOME,
        title: "fast-aws",
        aliases: ["all"],
        // No AWS calls, so nothing to reload on a region switch.
        scope: Scope::Global,
        columns: COLUMNS,
    }

    async fn list(&self, _: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
        let rows = self
            .entries
            .iter()
            .map(|(id, title, aliases)| {
                let jump = aliases
                    .iter()
                    .map(|a| format!(":{a}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                Row::new(*id, vec![(*title).into(), jump.into()])
            })
            .collect();
        Ok(Page { rows, next: None })
    }

    async fn describe(&self, _: &Ctx, _: Option<&Parent>, id: &str) -> Result<Detail, AwsError> {
        let fields = json!({ "Kind": id, "Open": "press Enter" });
        Ok(Detail {
            raw: id.to_owned(),
            fields,
        })
    }
}
