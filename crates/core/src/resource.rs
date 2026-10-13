//! The resource model. Each service describes its resource kinds with
//! [`ResourceKind`]; the UI only ever talks to that trait, which is what keeps the
//! keys and views the same for every service.

use std::sync::Arc;

use async_trait::async_trait;

use crate::context::Ctx;
use crate::error::AwsError;

/// Where a resource kind's API lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Uses the current region.
    Regional,
    /// One account-wide API (IAM, CloudFront); the region switcher is greyed out.
    Global,
    /// Always called in this region, whatever the current one (Cost Explorer).
    Pinned(&'static str),
}

/// How much harm an action can do. See the safety classes in `docs/plan/README.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Safety {
    Safe,
    /// Doesn't change AWS configuration but runs real code or sends real messages.
    SideEffect,
    Mutating,
    Destructive,
}

impl Safety {
    /// Whether actions of this class are offered in read-only mode.
    pub fn allowed_in_read_only(self) -> bool {
        matches!(self, Self::Safe | Self::SideEffect)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnWidth {
    /// Exactly this many cells.
    Fixed(u16),
    /// A share of the space left over after fixed columns.
    Fill(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub title: &'static str,
    pub width: ColumnWidth,
}

impl Column {
    pub const fn fixed(title: &'static str, width: u16) -> Self {
        Self {
            title,
            width: ColumnWidth::Fixed(width),
        }
    }

    pub const fn fill(title: &'static str, weight: u16) -> Self {
        Self {
            title,
            width: ColumnWidth::Fill(weight),
        }
    }
}

/// A hint for how a cell is coloured; the UI maps it to the theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Normal,
    Good,
    Warn,
    Bad,
    Dim,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub tone: Tone,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Normal,
        }
    }

    pub fn toned(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// One line in a resource list: a stable ID plus one cell per column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub cells: Vec<Cell>,
}

impl Row {
    /// Whether any cell contains `needle` (already lower-cased), for the `/` filter.
    pub fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.id.to_lowercase().contains(needle)
            || self
                .cells
                .iter()
                .any(|c| c.text.to_lowercase().contains(needle))
    }
}

/// A request for one page of a list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// The parent resource, for kinds that live inside another (objects in a bucket).
    pub parent: Option<String>,
    /// The continuation token from the previous page.
    pub next: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub rows: Vec<Row>,
    /// Set when there are more pages.
    pub next: Option<String>,
}

/// The detail view of one resource: a hand-built JSON tree of the useful fields, plus
/// the SDK's `Debug` output for everything else (SDK types don't implement serde).
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub fields: serde_json::Value,
    pub raw: String,
}

/// A list-able type of AWS resource, such as EC2 instances.
#[async_trait]
pub trait ResourceKind: Send + Sync {
    /// A stable ID, `service:kind`.
    fn id(&self) -> &'static str;
    /// Shown in the header and palette.
    fn title(&self) -> &'static str;
    /// Names accepted by the `:` palette, besides the ID.
    fn aliases(&self) -> &'static [&'static str];
    fn scope(&self) -> Scope;
    fn columns(&self) -> &'static [Column];

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError>;
    async fn describe(&self, ctx: &Ctx, id: &str) -> Result<Detail, AwsError>;
}

/// All resource kinds the binary was built with.
#[derive(Clone, Default)]
pub struct Registry {
    kinds: Vec<Arc<dyn ResourceKind>>,
}

impl Registry {
    pub fn register(&mut self, kind: impl ResourceKind + 'static) {
        self.kinds.push(Arc::new(kind));
    }

    pub fn kinds(&self) -> &[Arc<dyn ResourceKind>] {
        &self.kinds
    }

    /// Resolves what was typed in the palette: an exact ID or alias first, then the
    /// first kind whose ID, alias or title starts with it.
    pub fn find(&self, typed: &str) -> Option<Arc<dyn ResourceKind>> {
        let typed = typed.trim().trim_start_matches(':').to_lowercase();
        if typed.is_empty() {
            return None;
        }
        let names = |k: &Arc<dyn ResourceKind>| {
            let mut names = vec![k.id().to_owned(), k.title().to_lowercase()];
            names.extend(k.aliases().iter().map(|a| a.to_string()));
            names
        };
        let exact = self.kinds.iter().find(|k| names(k).contains(&typed));
        exact
            .or_else(|| {
                self.kinds
                    .iter()
                    .find(|k| names(k).iter().any(|n| n.starts_with(&typed)))
            })
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(&'static str, &'static str, &'static [&'static str]);

    #[async_trait]
    impl ResourceKind for Fake {
        fn id(&self) -> &'static str {
            self.0
        }
        fn title(&self) -> &'static str {
            self.1
        }
        fn aliases(&self) -> &'static [&'static str] {
            self.2
        }
        fn scope(&self) -> Scope {
            Scope::Regional
        }
        fn columns(&self) -> &'static [Column] {
            &[]
        }
        async fn list(&self, _: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(&self, _: &Ctx, _: &str) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    fn registry() -> Registry {
        let mut r = Registry::default();
        r.register(Fake(
            "ec2:instance",
            "EC2 Instances",
            &["ec2", "i", "instances"],
        ));
        r.register(Fake("ec2:sg", "Security Groups", &["sg"]));
        r
    }

    #[test]
    fn palette_prefers_exact_matches() {
        let r = registry();
        assert_eq!(r.find(":ec2").unwrap().id(), "ec2:instance");
        assert_eq!(r.find("sg").unwrap().id(), "ec2:sg");
        assert_eq!(r.find("ec2:sg").unwrap().id(), "ec2:sg");
    }

    #[test]
    fn palette_falls_back_to_prefix() {
        let r = registry();
        assert_eq!(r.find("inst").unwrap().id(), "ec2:instance");
        assert_eq!(r.find("secu").unwrap().id(), "ec2:sg");
        assert!(r.find("lambda").is_none());
        assert!(r.find(":").is_none());
    }

    #[test]
    fn read_only_hides_mutating_and_destructive() {
        assert!(Safety::Safe.allowed_in_read_only());
        assert!(Safety::SideEffect.allowed_in_read_only());
        assert!(!Safety::Mutating.allowed_in_read_only());
        assert!(!Safety::Destructive.allowed_in_read_only());
    }

    #[test]
    fn row_filter_is_case_insensitive_over_id_and_cells() {
        let row = Row {
            id: "i-0abc".into(),
            cells: vec!["Web-1".into(), "running".into()],
        };
        assert!(row.matches("web"));
        assert!(row.matches("i-0a"));
        assert!(row.matches(""));
        assert!(!row.matches("stopped"));
    }
}
