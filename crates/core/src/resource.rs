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

/// The resource a child list lives in: the bucket of an object list, the log group
/// of a stream list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parent {
    /// What the child kind needs to make its calls (a bucket name, `bucket/prefix/`,
    /// a log group name, an API ID). Its format is private to the two kinds.
    pub id: String,
    /// Shown in the breadcrumb.
    pub label: String,
    /// The region the child calls must go to, when it isn't the current one
    /// (S3 buckets live in one region each).
    pub region: Option<String>,
}

/// Where `Enter` on a row leads: a list of `kind` inside `parent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drill {
    pub kind: &'static str,
    pub parent: Parent,
}

/// One line in a resource list: a stable ID plus one cell per column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub cells: Vec<Cell>,
    /// Set when the row contains other resources; `Enter` opens them.
    pub drill: Option<Drill>,
    /// The region the resource lives in, when its calls can't use the current one
    /// (S3 buckets). Describe goes there.
    pub region: Option<String>,
}

impl Row {
    pub fn new(id: impl Into<String>, cells: Vec<Cell>) -> Self {
        Self {
            id: id.into(),
            cells,
            drill: None,
            region: None,
        }
    }

    /// Sends describe calls for this row to `region`.
    pub fn in_region(mut self, region: Option<String>) -> Self {
        self.region = region;
        self
    }

    /// Makes `Enter` on this row open a list of `kind` inside `parent`.
    pub fn drill(mut self, kind: &'static str, parent: Parent) -> Self {
        self.drill = Some(Drill { kind, parent });
        self
    }

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
    pub parent: Option<Parent>,
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

    /// Whether the kind can be opened from the `:` palette. Kinds that only make
    /// sense inside a parent (objects in a bucket) return `false`.
    fn in_palette(&self) -> bool {
        true
    }

    async fn list(&self, ctx: &Ctx, query: &ListQuery) -> Result<Page, AwsError>;
    async fn describe(
        &self,
        ctx: &Ctx,
        parent: Option<&Parent>,
        id: &str,
    ) -> Result<Detail, AwsError>;
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

    /// The kinds offered by the `:` palette.
    pub fn palette(&self) -> impl Iterator<Item = &Arc<dyn ResourceKind>> {
        self.kinds.iter().filter(|k| k.in_palette())
    }

    /// Looks a kind up by its ID, including kinds hidden from the palette.
    pub fn get(&self, id: &str) -> Option<Arc<dyn ResourceKind>> {
        self.kinds.iter().find(|k| k.id() == id).cloned()
    }

    /// The palette kinds matching what was typed, best first: an exact ID, alias
    /// or title; then a name starting with it; then a title or ID containing it.
    /// Ties keep registration order. Empty input lists every palette kind.
    pub fn search(&self, typed: &str) -> Vec<Arc<dyn ResourceKind>> {
        let typed = typed.trim().trim_start_matches(':').to_lowercase();
        let rank = |k: &Arc<dyn ResourceKind>| -> Option<u8> {
            if typed.is_empty() {
                return Some(0);
            }
            let title = k.title().to_lowercase();
            let names = || {
                [k.id(), title.as_str()]
                    .into_iter()
                    .chain(k.aliases().iter().copied())
            };
            if names().any(|n| n == typed) {
                Some(0)
            } else if names().any(|n| n.starts_with(&typed)) {
                Some(1)
            } else if title.contains(&typed) || k.id().contains(&typed) {
                Some(2)
            } else {
                None
            }
        };
        let mut ranked: Vec<_> = self
            .palette()
            .filter_map(|k| Some((rank(k)?, k.clone())))
            .collect();
        ranked.sort_by_key(|(r, _)| *r);
        ranked.into_iter().map(|(_, k)| k).collect()
    }

    /// The best palette match for what was typed, if any.
    pub fn find(&self, typed: &str) -> Option<Arc<dyn ResourceKind>> {
        if typed.trim().trim_start_matches(':').is_empty() {
            return None;
        }
        self.search(typed).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(&'static str, &'static str, &'static [&'static str], bool);

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
        fn in_palette(&self) -> bool {
            self.3
        }
        async fn list(&self, _: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(&self, _: &Ctx, _: Option<&Parent>, _: &str) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    fn registry() -> Registry {
        let mut r = Registry::default();
        r.register(Fake(
            "ec2:instance",
            "EC2 Instances",
            &["ec2", "i", "instances"],
            true,
        ));
        r.register(Fake("ec2:sg", "Security Groups", &["sg"], true));
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
    fn child_kinds_are_hidden_from_the_palette_but_can_be_looked_up() {
        let mut r = registry();
        r.register(Fake("s3:object", "Objects", &["objects"], false));
        assert!(r.find("objects").is_none());
        assert!(r.find("s3:object").is_none());
        assert_eq!(r.get("s3:object").unwrap().title(), "Objects");
        assert_eq!(r.palette().count(), 2);
    }

    #[test]
    fn search_ranks_exact_then_prefix_then_contains() {
        let mut r = registry();
        r.register(Fake("ec2:vpc", "VPCs", &["vpc"], true));
        r.register(Fake("ec2:sg-rule", "SG Rules", &["sgr"], true));
        let ids = |typed: &str| r.search(typed).iter().map(|k| k.id()).collect::<Vec<_>>();
        assert_eq!(ids("sg"), ["ec2:sg", "ec2:sg-rule"]);
        assert_eq!(ids("group"), ["ec2:sg"]);
        assert_eq!(
            ids(""),
            ["ec2:instance", "ec2:sg", "ec2:vpc", "ec2:sg-rule"]
        );
        assert!(ids("zzz").is_empty());
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
        let row = Row::new("i-0abc", vec!["Web-1".into(), "running".into()]);
        assert!(row.matches("web"));
        assert!(row.matches("i-0a"));
        assert!(row.matches(""));
        assert!(!row.matches("stopped"));
    }
}
