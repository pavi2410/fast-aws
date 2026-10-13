//! State of a resource list: loaded rows, the `/` filter, the selection, and
//! loading progress. Rendering lives in `render.rs`.

use std::sync::Arc;

use fast_aws_core::{AwsError, ResourceKind, Row};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    /// Pages are still arriving.
    Loading,
    Done,
    Failed(AwsError),
}

pub struct ListView {
    pub kind: Arc<dyn ResourceKind>,
    /// Identifies the load in flight; pages from older loads are ignored.
    pub generation: u64,
    pub status: LoadStatus,
    pub filter: String,
    rows: Vec<Row>,
    /// Indices into `rows` that pass the filter.
    visible: Vec<usize>,
    /// Index into `visible`.
    selected: usize,
    /// Set by a refresh: the old rows stay on screen until the first new page arrives.
    replace_on_next_page: bool,
}

impl ListView {
    pub fn new(kind: Arc<dyn ResourceKind>, generation: u64) -> Self {
        Self {
            kind,
            generation,
            status: LoadStatus::Loading,
            filter: String::new(),
            rows: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            replace_on_next_page: false,
        }
    }

    /// Starts a reload under a new generation, keeping the current rows visible.
    pub fn reload(&mut self, generation: u64) {
        self.generation = generation;
        self.status = LoadStatus::Loading;
        self.replace_on_next_page = true;
    }

    /// Adds a page of rows. The cursor stays on the same resource ID if it's still there.
    pub fn push_page(&mut self, rows: Vec<Row>, last: bool) {
        let selected_id = self.selected_row().map(|r| r.id.clone());
        if std::mem::take(&mut self.replace_on_next_page) {
            self.rows.clear();
        }
        self.rows.extend(rows);
        if last {
            self.status = LoadStatus::Done;
        }
        self.refilter(selected_id.as_deref());
    }

    pub fn fail(&mut self, err: AwsError) {
        self.replace_on_next_page = false;
        self.status = LoadStatus::Failed(err);
    }

    pub fn set_filter(&mut self, filter: &str) {
        let selected_id = self.selected_row().map(|r| r.id.clone());
        self.filter = filter.to_owned();
        self.refilter(selected_id.as_deref());
    }

    fn refilter(&mut self, keep_id: Option<&str>) {
        let needle = self.filter.to_lowercase();
        self.visible = (0..self.rows.len())
            .filter(|&i| self.rows[i].matches(&needle))
            .collect();
        self.selected = keep_id
            .and_then(|id| self.visible.iter().position(|&i| self.rows[i].id == id))
            .unwrap_or(self.selected)
            .min(self.visible.len().saturating_sub(1));
    }

    pub fn is_loading(&self) -> bool {
        self.status == LoadStatus::Loading
    }

    pub fn total(&self) -> usize {
        self.rows.len()
    }

    pub fn visible_rows(&self) -> impl ExactSizeIterator<Item = &Row> {
        self.visible.iter().map(|&i| &self.rows[i])
    }

    pub fn selected_index(&self) -> Option<usize> {
        (!self.visible.is_empty()).then_some(self.selected)
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.visible.get(self.selected).map(|&i| &self.rows[i])
    }

    /// Moves the cursor by `delta` rows, clamped to the list.
    pub fn move_by(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use async_trait::async_trait;
    use fast_aws_core::{Column, Ctx, Detail, ErrorKind, ListQuery, Page, Scope};

    pub(crate) struct FakeKind;

    #[async_trait]
    impl ResourceKind for FakeKind {
        fn id(&self) -> &'static str {
            "fake:thing"
        }
        fn title(&self) -> &'static str {
            "Things"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["thing"]
        }
        fn scope(&self) -> Scope {
            Scope::Regional
        }
        fn columns(&self) -> &'static [Column] {
            const COLUMNS: &[Column] = &[Column::fill("NAME", 1), Column::fixed("ID", 6)];
            COLUMNS
        }
        async fn list(&self, _: &Ctx, _: &ListQuery) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(&self, _: &Ctx, _: &str) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    pub(crate) fn row(id: &str, name: &str) -> Row {
        Row {
            id: id.into(),
            cells: vec![name.into(), id.into()],
        }
    }

    fn view() -> ListView {
        let mut v = ListView::new(Arc::new(FakeKind), 1);
        v.push_page(vec![row("a", "web-1"), row("b", "web-2")], false);
        v.push_page(vec![row("c", "db")], true);
        v
    }

    fn ids(v: &ListView) -> Vec<&str> {
        v.visible_rows().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn pages_accumulate_until_the_last() {
        let mut v = ListView::new(Arc::new(FakeKind), 1);
        v.push_page(vec![row("a", "x")], false);
        assert!(v.is_loading());
        v.push_page(vec![row("b", "y")], true);
        assert_eq!(v.status, LoadStatus::Done);
        assert_eq!(ids(&v), ["a", "b"]);
    }

    #[test]
    fn filter_keeps_the_cursor_on_the_same_resource() {
        let mut v = view();
        v.move_by(1);
        assert_eq!(v.selected_row().unwrap().id, "b");
        v.set_filter("WEB");
        assert_eq!(ids(&v), ["a", "b"]);
        assert_eq!(v.selected_row().unwrap().id, "b");
        v.set_filter("db");
        assert_eq!(ids(&v), ["c"]);
        assert_eq!(v.selected_row().unwrap().id, "c");
        v.set_filter("nothing");
        assert!(v.selected_row().is_none());
        assert_eq!(v.selected_index(), None);
    }

    #[test]
    fn movement_is_clamped() {
        let mut v = view();
        v.move_by(-5);
        assert_eq!(v.selected_row().unwrap().id, "a");
        v.move_by(10);
        assert_eq!(v.selected_row().unwrap().id, "c");
        v.select_first();
        assert_eq!(v.selected_row().unwrap().id, "a");
        v.select_last();
        assert_eq!(v.selected_row().unwrap().id, "c");
    }

    #[test]
    fn reload_keeps_old_rows_until_the_first_new_page() {
        let mut v = view();
        v.move_by(2);
        v.reload(2);
        assert_eq!(ids(&v), ["a", "b", "c"]);
        assert!(v.is_loading());
        v.push_page(vec![row("c", "db"), row("d", "new")], true);
        assert_eq!(ids(&v), ["c", "d"]);
        assert_eq!(v.selected_row().unwrap().id, "c");
    }

    #[test]
    fn a_failed_reload_keeps_the_old_rows() {
        let mut v = view();
        v.reload(2);
        v.fail(AwsError::new(ErrorKind::Throttled, None, "slow down"));
        assert_eq!(v.total(), 3);
        assert!(matches!(v.status, LoadStatus::Failed(_)));
    }
}
