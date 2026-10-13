//! The app state machine. Key presses and background results go in; state changes
//! and [`Effect`]s (work for the runtime to start) come out. Nothing here touches
//! the terminal or AWS, so all of it is unit-tested.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fast_aws_core::metrics::MetricSpec;
use fast_aws_core::profiles::{ProfileCatalog, ProfileInfo};
use fast_aws_core::{
    AwsError, CallerIdentity, Config, Detail, ErrorKind, Guard, Page, Parent, Query, Registry,
    Relation, ResourceKind, Scope,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cache::{Cache, CacheKey};
use crate::detail::{DetailState, DetailView};
use crate::list::{ListView, LoadStatus};

/// Regions offered by `R`. A live list from `DescribeRegions` comes with the Phase 2 cache.
pub const REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "ca-central-1",
    "sa-east-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-central-1",
    "eu-central-2",
    "eu-north-1",
    "eu-south-1",
    "ap-south-1",
    "ap-south-2",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "me-central-1",
    "af-south-1",
];

const PAGE_JUMP: isize = 20;

/// Rows around the cursor whose sparklines are fetched together.
const SPARK_BATCH: usize = 60;
/// How long fetched sparklines are reused before they're fetched again.
const SPARK_TTL: Duration = Duration::from_secs(600);

/// Profile, region and kind a sparkline request was made for.
pub type SparkScope = (String, String, &'static str);
type SparkKey = (String, String, &'static str, String);

struct Spark {
    /// `None` while being fetched.
    points: Option<Vec<f64>>,
    at: Instant,
}

/// Work for the runtime. Loads run against the app's current profile and region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Check the credentials. Runs apart from other loads: `CancelLoads` doesn't
    /// stop it; only a newer identity check replaces it.
    LoadIdentity {
        generation: u64,
    },
    /// Load a list from its start, or continue it `from` a page token. Loading
    /// stops after a few pages and reports `Msg::Paused`.
    LoadList {
        generation: u64,
        from: Option<String>,
    },
    LoadDetail {
        generation: u64,
        id: String,
    },
    /// Abort every running list and detail load (the view or context changed).
    CancelLoads,
    /// Suspend the TUI and run this command (SSO login) in the terminal.
    RunInTerminal(Vec<String>),
    /// Fetch sparkline points for these rows (billed per metric).
    LoadMetrics {
        scope: SparkScope,
        metrics: Vec<(String, MetricSpec)>,
    },
    Quit,
}

/// Results coming back from the runtime.
#[derive(Debug)]
pub enum Msg {
    Metrics {
        scope: SparkScope,
        result: Result<HashMap<String, Vec<f64>>, AwsError>,
    },
    Identity {
        generation: u64,
        result: Result<CallerIdentity, AwsError>,
    },
    Page {
        generation: u64,
        result: Result<Page, AwsError>,
    },
    /// The list hit the page limit; there's more from `next`.
    Paused {
        generation: u64,
        next: String,
    },
    Detail {
        generation: u64,
        result: Result<Detail, AwsError>,
    },
    TerminalCommandFinished {
        success: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityState {
    Loading,
    Ready(CallerIdentity),
    Failed(AwsError),
}

pub enum Screen {
    List,
    Detail(DetailView),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// `:` command palette: what has been typed, and the highlighted match.
    Palette(String, usize),
    /// `/` filter being edited (the text lives in the list).
    Filter,
    /// `P`: index into [`App::profile_entries`].
    Profiles(usize),
    /// `R`: index into [`REGIONS`].
    Regions(usize),
    /// `g`: index into [`App::relations_menu`].
    Relations(usize),
    /// A row's question being answered, with what has been typed.
    Prompt(Query, String),
    Help,
}

/// A line in the profile picker: an sso-session heading or a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerEntry {
    Heading(String),
    Profile(String),
}

pub struct App {
    pub registry: Registry,
    pub catalog: ProfileCatalog,
    pub profile: String,
    pub region: String,
    pub identity: IdentityState,
    /// The list on screen.
    pub list: ListView,
    /// Lists drilled out of, outermost first; `Esc` returns to the last one.
    pub back: Vec<ListView>,
    pub screen: Screen,
    pub overlay: Overlay,
    /// A one-line message in the footer, cleared by the next key.
    pub flash: Option<String>,
    pub should_quit: bool,
    /// Which action classes the current profile allows; see [`Guard`].
    pub guard: Guard,
    /// What the open `g` menu lists: the row's own relations, then reverse ones
    /// found in cached lists.
    pub relations_menu: Vec<Relation>,
    cache: Cache,
    metrics_enabled: bool,
    sparks: HashMap<SparkKey, Spark>,
    config: Config,
    read_only_flag: bool,
    next_generation: u64,
    /// The identity check whose result is wanted; older results are ignored.
    identity_generation: u64,
}

impl App {
    /// Creates the app showing `kind`, and returns the loads to start.
    pub fn new(
        registry: Registry,
        catalog: ProfileCatalog,
        profile: String,
        region: String,
        kind: Arc<dyn ResourceKind>,
    ) -> (Self, Vec<Effect>) {
        let app = Self {
            registry,
            catalog,
            profile,
            region,
            identity: IdentityState::Loading,
            list: ListView::new(kind, 1),
            back: Vec::new(),
            screen: Screen::List,
            overlay: Overlay::None,
            flash: None,
            should_quit: false,
            guard: Guard::new(&Config::default(), "", false),
            cache: Cache::default(),
            relations_menu: Vec::new(),
            metrics_enabled: false,
            sparks: HashMap::new(),
            config: Config::default(),
            read_only_flag: false,
            next_generation: 2,
            identity_generation: 1,
        };
        (
            app,
            vec![
                Effect::LoadIdentity { generation: 1 },
                Effect::LoadList {
                    generation: 1,
                    from: None,
                },
            ],
        )
    }

    /// Applies the config file and `--read-only` to the current profile and to
    /// every profile switched to later.
    pub fn set_safety(&mut self, config: Config, read_only_flag: bool) {
        self.config = config;
        self.read_only_flag = read_only_flag;
        self.guard = Guard::new(&self.config, &self.profile, read_only_flag);
    }

    fn generation(&mut self) -> u64 {
        let g = self.next_generation;
        self.next_generation += 1;
        g
    }

    pub fn current_profile(&self) -> Option<&ProfileInfo> {
        self.catalog.get(&self.profile)
    }

    /// Profiles grouped under their sso-session headings, as shown by `P`.
    pub fn profile_entries(&self) -> Vec<PickerEntry> {
        let mut entries = Vec::new();
        for group in self.catalog.groups() {
            let heading = match group.sso_session {
                Some(s) => format!("sso-session {s}"),
                None => "other profiles".to_owned(),
            };
            entries.push(PickerEntry::Heading(heading));
            entries.extend(
                group
                    .profiles
                    .iter()
                    .map(|p| PickerEntry::Profile(p.name.clone())),
            );
        }
        entries
    }

    /// The error that explains why nothing useful is on screen, if any: a failed
    /// identity check outranks a failed list, since it's usually the cause.
    pub fn blocking_error(&self) -> Option<&AwsError> {
        match (&self.identity, &self.list.status) {
            (IdentityState::Failed(e), _) => Some(e),
            (_, crate::list::LoadStatus::Failed(e)) => Some(e),
            _ => None,
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) -> Vec<Effect> {
        let mut effects = self.handle_msg_inner(msg);
        effects.extend(self.sparklines_effect());
        effects
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let mut effects = self.handle_key_inner(key);
        effects.extend(self.sparklines_effect());
        effects
    }

    /// Turns sparkline fetching on (the binary does, unless `--no-metrics`).
    pub fn set_metrics(&mut self, enabled: bool) {
        self.metrics_enabled = enabled;
    }

    /// The fetched sparkline for a row of the current list, if any: `None` when
    /// not fetched, `Some(None)` while being fetched.
    pub fn sparkline(&self, row_id: &str) -> Option<Option<&[f64]>> {
        let key = self.spark_key(row_id);
        self.sparks.get(&key).map(|s| s.points.as_deref())
    }

    fn spark_key(&self, row_id: &str) -> SparkKey {
        (
            self.profile.clone(),
            self.region.clone(),
            self.list.kind.id(),
            row_id.to_owned(),
        )
    }

    /// Requests sparklines for rows near the cursor that don't have fresh ones:
    /// at most [`SPARK_BATCH`] per request, since each metric is billed.
    fn sparklines_effect(&mut self) -> Option<Effect> {
        if !self.metrics_enabled || self.list.is_loading() || !matches!(self.screen, Screen::List) {
            return None;
        }
        let at = self.list.selected_index().unwrap_or(0);
        let from = at.saturating_sub(SPARK_BATCH / 2);
        let wanted: Vec<(String, MetricSpec)> = self
            .list
            .visible_rows()
            .skip(from)
            .take(SPARK_BATCH)
            .filter_map(|r| Some((r.id.clone(), r.metric.clone()?)))
            .filter(|(id, _)| {
                self.sparks
                    .get(&self.spark_key(id))
                    .is_none_or(|s| s.at.elapsed() > SPARK_TTL)
            })
            .collect();
        if wanted.is_empty() {
            return None;
        }
        for (id, _) in &wanted {
            let key = self.spark_key(id);
            self.sparks.insert(
                key,
                Spark {
                    points: None,
                    at: Instant::now(),
                },
            );
        }
        let scope = (
            self.profile.clone(),
            self.region.clone(),
            self.list.kind.id(),
        );
        Some(Effect::LoadMetrics {
            scope,
            metrics: wanted,
        })
    }

    fn handle_msg_inner(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Identity { generation, result } if generation == self.identity_generation => {
                self.identity = match result {
                    Ok(id) => IdentityState::Ready(id),
                    Err(e) => IdentityState::Failed(e),
                };
            }
            Msg::Page { generation, result } if generation == self.list.generation => {
                match result {
                    Ok(page) => {
                        let following = self.list.kind.follow_every().is_some();
                        let at_end = self
                            .list
                            .selected_index()
                            .is_none_or(|i| i + 1 >= self.list.visible_rows().len());
                        // A followed list never finishes; its pages only add.
                        let last = page.next.is_none() && !following;
                        self.list.push_page(page.rows, last);
                        if following && at_end {
                            self.list.select_last();
                        }
                        self.remember_list();
                    }
                    Err(e) => self.list.fail(e),
                }
            }
            Msg::Paused { generation, next } if generation == self.list.generation => {
                self.list.pause(next);
                self.remember_list();
            }
            Msg::Detail { generation, result } => {
                if let Screen::Detail(d) = &mut self.screen
                    && d.generation == generation
                {
                    d.state = match result {
                        Ok(detail) => DetailState::Ready(detail),
                        Err(e) => DetailState::Failed(e),
                    };
                }
            }
            Msg::Metrics { scope, result } => {
                let (profile, region, kind) = scope;
                match result {
                    Ok(points) => {
                        for (id, series) in points {
                            let key = (profile.clone(), region.clone(), kind, id);
                            self.sparks.insert(
                                key,
                                Spark {
                                    points: Some(series),
                                    at: Instant::now(),
                                },
                            );
                        }
                    }
                    // The rows stay marked as fetched, so this isn't retried
                    // until they go stale.
                    Err(e) => self.flash = Some(format!("sparklines unavailable: {e}")),
                }
            }
            Msg::TerminalCommandFinished { success } => {
                if !success {
                    self.flash = Some("login command failed; see its output above".into());
                }
                return self.reload_context(true);
            }
            // From a load that was replaced.
            Msg::Identity { .. } | Msg::Page { .. } | Msg::Paused { .. } => {}
        }
        Vec::new()
    }

    fn handle_key_inner(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![Effect::Quit];
        }
        self.flash = None;
        match self.overlay.clone() {
            Overlay::Palette(input, at) => self.palette_key(key, input, at),
            Overlay::Filter => self.filter_key(key),
            Overlay::Profiles(i) => self.profiles_key(key, i),
            Overlay::Regions(i) => self.regions_key(key, i),
            Overlay::Relations(i) => self.relations_key(key, i),
            Overlay::Prompt(query, input) => self.prompt_key(key, query, input),
            Overlay::Help => {
                self.overlay = Overlay::None;
                Vec::new()
            }
            Overlay::None => match self.screen {
                Screen::List => self.list_key(key),
                Screen::Detail(_) => self.detail_key(key),
            },
        }
    }

    /// Keys that work the same on every screen. Returns `None` if `key` isn't one.
    fn global_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let effects = match key.code {
            KeyCode::Char(':') => {
                self.overlay = Overlay::Palette(String::new(), 0);
                Vec::new()
            }
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
                Vec::new()
            }
            KeyCode::Char('P') => {
                let entries = self.profile_entries();
                let at = entries
                    .iter()
                    .position(|e| *e == PickerEntry::Profile(self.profile.clone()))
                    .or_else(|| {
                        entries
                            .iter()
                            .position(|e| matches!(e, PickerEntry::Profile(_)))
                    });
                match at {
                    Some(i) => self.overlay = Overlay::Profiles(i),
                    None => self.flash = Some("no profiles found in ~/.aws/config".into()),
                }
                Vec::new()
            }
            KeyCode::Char('R') => {
                let i = REGIONS.iter().position(|r| *r == self.region).unwrap_or(0);
                self.overlay = Overlay::Regions(i);
                Vec::new()
            }
            KeyCode::Char('L') => self.login(),
            KeyCode::Char('g') => {
                self.relations_menu = self.relations_of_selected();
                if self.relations_menu.is_empty() {
                    self.flash = Some("nothing related to go to from here".into());
                } else {
                    self.overlay = Overlay::Relations(0);
                }
                Vec::new()
            }
            _ => return None,
        };
        Some(effects)
    }

    fn list_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(effects) = self.global_key(key) {
            return effects;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.list.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.list.move_by(-1),
            KeyCode::PageDown => self.list.move_by(PAGE_JUMP),
            KeyCode::PageUp => self.list.move_by(-PAGE_JUMP),
            KeyCode::Home => self.list.select_first(),
            KeyCode::End => self.list.select_last(),
            KeyCode::Char('/') => self.overlay = Overlay::Filter,
            KeyCode::Char('r') => {
                let generation = self.generation();
                self.list.reload(generation);
                return vec![
                    Effect::CancelLoads,
                    Effect::LoadList {
                        generation,
                        from: None,
                    },
                ];
            }
            KeyCode::Char('n') => {
                if let Some(from) = self.list.resume() {
                    let generation = self.list.generation;
                    return vec![Effect::LoadList {
                        generation,
                        from: Some(from),
                    }];
                }
            }
            KeyCode::Enter => return self.open_selected(),
            KeyCode::Char('d') => return self.open_detail(),
            KeyCode::Esc if !self.list.filter.is_empty() => self.list.set_filter(""),
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') if !self.back.is_empty() => {
                return self.go_back();
            }
            KeyCode::Char('q') => {
                self.should_quit = true;
                return vec![Effect::Quit];
            }
            KeyCode::Char(c) => {
                let query = self
                    .list
                    .selected_row()
                    .and_then(|r| r.queries.iter().find(|q| q.key == c));
                if let Some(query) = query.cloned() {
                    self.overlay = Overlay::Prompt(query, String::new());
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn prompt_key(&mut self, key: KeyEvent, query: Query, mut input: String) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                let input = input.trim().to_owned();
                if input.is_empty() {
                    return Vec::new();
                }
                let Some(kind) = self.registry.get(query.kind) else {
                    self.flash = Some(format!("{} isn't available in this build", query.kind));
                    return Vec::new();
                };
                let (list, load) = self.list_for(kind, Some(query.parent(&input)));
                self.back.push(std::mem::replace(&mut self.list, list));
                return std::iter::once(Effect::CancelLoads).chain(load).collect();
            }
            KeyCode::Backspace => {
                input.pop();
                self.overlay = Overlay::Prompt(query, input);
            }
            KeyCode::Char(c) => {
                input.push(c);
                self.overlay = Overlay::Prompt(query, input);
            }
            _ => {}
        }
        Vec::new()
    }

    /// `Enter`: drills into the row if it contains other resources, else describes it.
    fn open_selected(&mut self) -> Vec<Effect> {
        let Some(drill) = self.list.selected_row().and_then(|r| r.drill.clone()) else {
            return self.open_detail();
        };
        let Some(kind) = self.registry.get(drill.kind) else {
            self.flash = Some(format!("{} isn't available in this build", drill.kind));
            return Vec::new();
        };
        let (child, load) = self.list_for(kind, Some(drill.parent));
        self.back.push(std::mem::replace(&mut self.list, child));
        std::iter::once(Effect::CancelLoads).chain(load).collect()
    }

    /// A list of `kind` in `parent`: from the cache when there is one (refreshed
    /// in the background unless the kind opts out), else empty. Also returns the
    /// load to start, if any.
    fn list_for(
        &mut self,
        kind: Arc<dyn ResourceKind>,
        parent: Option<Parent>,
    ) -> (ListView, Option<Effect>) {
        let generation = self.generation();
        let load = Effect::LoadList {
            generation,
            from: None,
        };
        let key = self.cache_key(kind.id(), parent.as_ref());
        match self.cache.get(&key) {
            Some(entry) => {
                let refresh = kind.refresh_on_open();
                let list = ListView::from_cache(kind, parent, generation, entry, refresh);
                (list, refresh.then_some(load))
            }
            None => (ListView::inside(kind, parent, generation), Some(load)),
        }
    }

    fn cache_key(&self, kind: &'static str, parent: Option<&Parent>) -> CacheKey {
        CacheKey {
            profile: self.profile.clone(),
            region: self.region.clone(),
            kind,
            parent: parent.map(|p| p.id.clone()),
        }
    }

    /// Stores the current list once it has stopped loading.
    fn remember_list(&mut self) {
        let unfinished =
            self.list.is_loading() || matches!(self.list.status, LoadStatus::Failed(_));
        if unfinished || !self.list.kind.cacheable() {
            return;
        }
        let key = self.cache_key(self.list.kind.id(), self.list.parent.as_ref());
        let more = self.list.more().map(str::to_owned);
        self.cache.put(key, self.list.rows().to_vec(), more);
    }

    /// Returns to the list drilled out of. It's reloaded if its load was cut short.
    fn go_back(&mut self) -> Vec<Effect> {
        let Some(previous) = self.back.pop() else {
            return Vec::new();
        };
        self.list = previous;
        let mut effects = vec![Effect::CancelLoads];
        if self.list.is_loading() {
            let generation = self.generation();
            self.list.reload(generation);
            effects.push(Effect::LoadList {
                generation,
                from: None,
            });
        }
        effects
    }

    /// Where the current list sits: the root kind's title, then each parent.
    ///
    /// A list inside a parent shows the parent (`S3 Buckets › logs › 2026/`); a
    /// list reached with `g` shows its own title (`Lambda Functions › IAM Roles`).
    pub fn breadcrumb(&self) -> Vec<&str> {
        self.back
            .iter()
            .chain(std::iter::once(&self.list))
            .map(|l| match &l.parent {
                Some(parent) => parent.label.as_str(),
                None => l.kind.title(),
            })
            .collect()
    }

    fn open_detail(&mut self) -> Vec<Effect> {
        let Some(id) = self.list.selected_row().map(|r| r.id.clone()) else {
            return Vec::new();
        };
        let generation = self.generation();
        self.screen = Screen::Detail(DetailView::new(
            self.breadcrumb().join(" › "),
            id.clone(),
            generation,
        ));
        vec![Effect::LoadDetail { generation, id }]
    }

    fn detail_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(effects) = self.global_key(key) {
            return effects;
        }
        let Screen::Detail(d) = &mut self.screen else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.screen = Screen::List,
            KeyCode::Char('j') | KeyCode::Down => d.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => d.scroll_by(-1),
            KeyCode::PageDown => d.scroll_by(PAGE_JUMP as i32),
            KeyCode::PageUp => d.scroll_by(-PAGE_JUMP as i32),
            KeyCode::Home => d.scroll = 0,
            KeyCode::Tab => {
                d.raw = !d.raw;
                d.scroll = 0;
            }
            KeyCode::Char('r') => {
                let id = d.id.clone();
                let generation = self.generation();
                if let Screen::Detail(d) = &mut self.screen {
                    d.generation = generation;
                    d.state = DetailState::Loading;
                }
                return vec![Effect::LoadDetail { generation, id }];
            }
            _ => {}
        }
        Vec::new()
    }

    fn palette_key(&mut self, key: KeyEvent, mut input: String, at: usize) -> Vec<Effect> {
        let matches = self.registry.search(&input).len();
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                return self.run_command(&input, at);
            }
            KeyCode::Down | KeyCode::Tab => {
                self.overlay = Overlay::Palette(input, (at + 1).min(matches.saturating_sub(1)));
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.overlay = Overlay::Palette(input, at.saturating_sub(1));
            }
            KeyCode::Backspace => {
                input.pop();
                self.overlay = Overlay::Palette(input, 0);
            }
            KeyCode::Char(c) => {
                input.push(c);
                self.overlay = Overlay::Palette(input, 0);
            }
            _ => {}
        }
        Vec::new()
    }

    /// Runs what was typed in the palette: `q` quits, anything else opens the
    /// highlighted match.
    fn run_command(&mut self, input: &str, at: usize) -> Vec<Effect> {
        let command = input.trim();
        if matches!(command, "q" | "quit") {
            self.should_quit = true;
            return vec![Effect::Quit];
        }
        if command == "unlock" {
            self.flash = Some(if self.guard.prod {
                self.guard.unlocked = true;
                format!("{} unlocked for this session", self.profile)
            } else {
                format!("{} isn't marked prod; nothing to unlock", self.profile)
            });
            return Vec::new();
        }
        let Some(kind) = self.registry.search(command).into_iter().nth(at) else {
            if !command.is_empty() {
                self.flash = Some(format!("unknown resource type: {command}"));
            }
            return Vec::new();
        };
        let (list, load) = self.list_for(kind, None);
        self.list = list;
        self.back.clear();
        self.screen = Screen::List;
        std::iter::once(Effect::CancelLoads).chain(load).collect()
    }

    fn filter_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let mut filter = self.list.filter.clone();
        match key.code {
            KeyCode::Enter => self.overlay = Overlay::None,
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                filter.clear();
            }
            KeyCode::Backspace => {
                filter.pop();
            }
            KeyCode::Char(c) => filter.push(c),
            _ => {}
        }
        self.list.set_filter(&filter);
        Vec::new()
    }

    fn profiles_key(&mut self, key: KeyEvent, at: usize) -> Vec<Effect> {
        let entries = self.profile_entries();
        let step = |from: usize, forward: bool| {
            let mut i = from;
            loop {
                i = match forward {
                    true if i + 1 < entries.len() => i + 1,
                    false if i > 0 => i - 1,
                    _ => return from,
                };
                if matches!(entries[i], PickerEntry::Profile(_)) {
                    return i;
                }
            }
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Char('j') | KeyCode::Down => self.overlay = Overlay::Profiles(step(at, true)),
            KeyCode::Char('k') | KeyCode::Up => self.overlay = Overlay::Profiles(step(at, false)),
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                if let Some(PickerEntry::Profile(name)) = entries.get(at)
                    && *name != self.profile
                {
                    self.profile = name.clone();
                    // A new profile starts locked, with its own settings.
                    self.guard = Guard::new(&self.config, &self.profile, self.read_only_flag);
                    if let Some(region) = self.current_profile().and_then(|p| p.region.clone()) {
                        self.region = region;
                    }
                    return self.reload_context(true);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// The selected row's relations, then reverse ones: rows in cached lists
    /// (same profile and region) whose relations point at it, such as the
    /// functions that use a role. Those are only as complete as what's loaded.
    fn relations_of_selected(&self) -> Vec<Relation> {
        let Some(row) = self.list.selected_row() else {
            return Vec::new();
        };
        let mut menu = row.relations.clone();
        let kind = self.list.kind.id();
        let names_this = |r: &Relation| {
            r.kind == kind
                && r.filter
                    .as_deref()
                    .is_some_and(|f| f == row.id || row.cells.first().is_some_and(|c| c.text == f))
        };
        for (key, entry) in self.cache.lists_for(&self.profile, &self.region) {
            let title = self.registry.get(key.kind).map_or(key.kind, |k| k.title());
            for source in &entry.rows {
                if source.relations.iter().any(names_this) {
                    let name = source
                        .cells
                        .first()
                        .map_or(source.id.as_str(), |c| c.text.as_str());
                    let parent = key.parent.as_ref().map(|id| Parent {
                        id: id.clone(),
                        label: id.clone(),
                        region: None,
                    });
                    menu.push(Relation {
                        label: format!("used by {name} ({title}, from loaded lists)"),
                        kind: key.kind,
                        parent,
                        filter: Some(source.id.clone()),
                    });
                }
            }
        }
        menu
    }

    fn relations_key(&mut self, key: KeyEvent, at: usize) -> Vec<Effect> {
        let count = self.relations_menu.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Char('j') | KeyCode::Down => {
                self.overlay = Overlay::Relations((at + 1).min(count.saturating_sub(1)));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.overlay = Overlay::Relations(at.saturating_sub(1))
            }
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                return self.follow(at);
            }
            _ => {}
        }
        Vec::new()
    }

    /// Jumps to the selected row's `at`-th relation. The current list goes on
    /// the back stack, so `Esc` returns to it.
    fn follow(&mut self, at: usize) -> Vec<Effect> {
        let Some(relation) = self.relations_menu.get(at).cloned() else {
            return Vec::new();
        };
        let Some(kind) = self.registry.get(relation.kind) else {
            self.flash = Some(format!("{} isn't available in this build", relation.kind));
            return Vec::new();
        };
        let (mut list, load) = self.list_for(kind, relation.parent);
        if let Some(filter) = &relation.filter {
            list.set_filter(filter);
        }
        self.back.push(std::mem::replace(&mut self.list, list));
        self.screen = Screen::List;
        std::iter::once(Effect::CancelLoads).chain(load).collect()
    }

    fn regions_key(&mut self, key: KeyEvent, at: usize) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Char('j') | KeyCode::Down => {
                self.overlay = Overlay::Regions((at + 1).min(REGIONS.len() - 1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.overlay = Overlay::Regions(at.saturating_sub(1))
            }
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                if REGIONS[at] != self.region {
                    self.region = REGIONS[at].to_owned();
                    // Global and pinned services (IAM, CloudFront, Cost Explorer)
                    // look the same from every region; reloading them would only
                    // repeat the calls (billed ones, for Cost Explorer).
                    let mut lists = self.back.iter().chain(std::iter::once(&self.list));
                    if lists.all(|l| l.kind.scope() != Scope::Regional) {
                        return Vec::new();
                    }
                    return self.reload_context(false);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Starts over in the current profile and region: back to the top-level list
    /// (drilled-into parents may not exist there), reloaded.
    fn reload_context(&mut self, identity_too: bool) -> Vec<Effect> {
        let root = self.back.first().unwrap_or(&self.list).kind.clone();
        let (list, load) = self.list_for(root, None);
        self.list = list;
        self.back.clear();
        self.screen = Screen::List;
        let mut effects = vec![Effect::CancelLoads];
        if identity_too {
            self.identity_generation += 1;
            self.identity = IdentityState::Loading;
            effects.push(Effect::LoadIdentity {
                generation: self.identity_generation,
            });
        }
        effects.extend(load);
        effects
    }

    fn login(&mut self) -> Vec<Effect> {
        match self.current_profile().and_then(ProfileInfo::login_command) {
            Some(command) => vec![Effect::RunInTerminal(command)],
            None => {
                self.flash = Some(format!(
                    "profile {} doesn't use SSO or aws login; nothing to log in to",
                    self.profile
                ));
                Vec::new()
            }
        }
    }

    /// Whether the visible error is one that `L` can fix.
    pub fn login_would_help(&self) -> bool {
        self.blocking_error()
            .is_some_and(|e| e.kind == ErrorKind::SessionExpired)
            && self
                .current_profile()
                .and_then(ProfileInfo::login_command)
                .is_some()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::list::LoadStatus;
    use crate::list::tests::{FakeChild, FakeKind, row};
    use fast_aws_core::profiles::Auth;
    use fast_aws_core::{Parent, Row};

    pub(crate) fn catalog() -> ProfileCatalog {
        let p = |name: &str, region: Option<&str>, auth: Auth| ProfileInfo {
            name: name.into(),
            region: region.map(Into::into),
            auth,
        };
        ProfileCatalog {
            profiles: vec![
                p(
                    "acme-dev",
                    Some("eu-west-1"),
                    Auth::SsoSession("acme".into()),
                ),
                p("acme-qa", None, Auth::SsoSession("acme".into())),
                p("ci", None, Auth::AssumeRole),
            ],
            selected: "acme-dev".into(),
        }
    }

    /// The app before any list has loaded.
    pub(crate) fn empty_app() -> App {
        let mut registry = Registry::default();
        registry.register(FakeKind);
        registry.register(FakeChild);
        let kind = registry.find("thing").unwrap();
        App::new(
            registry,
            catalog(),
            "acme-dev".into(),
            "eu-west-1".into(),
            kind,
        )
        .0
    }

    pub(crate) fn app() -> App {
        let mut registry = Registry::default();
        registry.register(FakeKind);
        registry.register(FakeChild);
        let kind = registry.find("thing").unwrap();
        let (mut app, effects) = App::new(
            registry,
            catalog(),
            "acme-dev".into(),
            "eu-west-1".into(),
            kind,
        );
        assert_eq!(
            effects,
            [
                Effect::LoadIdentity { generation: 1 },
                Effect::LoadList {
                    generation: 1,
                    from: None
                }
            ]
        );
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![
                    row("a", "web-1").drill("fake:part", part_parent()),
                    row("b", "db"),
                ],
                next: None,
            }),
        });
        app
    }

    fn part_parent() -> Parent {
        Parent {
            id: "a".into(),
            label: "web-1".into(),
            region: Some("us-west-2".into()),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(app: &mut App, keys: &str) -> Vec<Effect> {
        keys.chars()
            .flat_map(|c| app.handle_key(key(KeyCode::Char(c))))
            .collect()
    }

    #[test]
    fn stale_pages_are_ignored() {
        let mut app = app();
        let effects = press(&mut app, "r");
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadList {
                    generation: 2,
                    from: None
                }
            ]
        );
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![row("zzz", "old")],
                next: None,
            }),
        });
        assert_eq!(app.list.total(), 2);
        assert!(app.list.is_loading());
    }

    #[test]
    fn n_continues_a_paused_list_under_the_same_generation() {
        let mut app = app();
        press(&mut app, "r");
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Ok(Page {
                rows: vec![row("a", "x")],
                next: Some("t2".into()),
            }),
        });
        app.handle_msg(Msg::Paused {
            generation: 2,
            next: "t2".into(),
        });
        assert_eq!(
            press(&mut app, "n"),
            [Effect::LoadList {
                generation: 2,
                from: Some("t2".into())
            }]
        );
        assert!(press(&mut app, "n").is_empty()); // already loading
    }

    #[test]
    fn enter_opens_detail_and_esc_returns() {
        let mut app = app();
        press(&mut app, "j");
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            effects,
            [Effect::LoadDetail {
                generation: 2,
                id: "b".into()
            }]
        );
        assert!(matches!(&app.screen, Screen::Detail(d) if d.id == "b"));
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.screen, Screen::List));
        assert_eq!(app.list.selected_row().unwrap().id, "b");
    }

    #[test]
    fn enter_drills_into_rows_with_children_and_esc_comes_back() {
        let mut app = app();
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadList {
                    generation: 2,
                    from: None
                }
            ]
        );
        assert_eq!(app.list.kind.id(), "fake:part");
        assert_eq!(app.list.parent, Some(part_parent()));
        assert_eq!(app.breadcrumb(), ["Things", "web-1"]);

        // The parent list had finished loading, so going back needs no reload.
        assert_eq!(app.handle_key(key(KeyCode::Esc)), [Effect::CancelLoads]);
        assert_eq!(app.list.kind.id(), "fake:thing");
        assert_eq!(app.list.selected_row().unwrap().id, "a");
        assert_eq!(app.breadcrumb(), ["Things"]);
    }

    #[test]
    fn going_back_reloads_a_parent_whose_load_was_cut_short() {
        let mut app = app();
        press(&mut app, "r"); // generation 2, still loading
        app.handle_key(key(KeyCode::Enter)); // generation 3
        let effects = press(&mut app, "q");
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadList {
                    generation: 4,
                    from: None
                }
            ]
        );
        assert_eq!(app.list.kind.id(), "fake:thing");
        assert!(!app.should_quit);
    }

    #[test]
    fn d_describes_even_a_row_with_children() {
        let mut app = app();
        let effects = press(&mut app, "d");
        assert_eq!(
            effects,
            [Effect::LoadDetail {
                generation: 2,
                id: "a".into()
            }]
        );
    }

    #[test]
    fn switching_region_inside_a_child_returns_to_the_top_level_list() {
        let mut app = app();
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "Rj");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.list.kind.id(), "fake:thing");
        assert!(app.back.is_empty());
        assert_eq!(app.list.parent, None);
    }

    #[test]
    fn palette_opens_the_highlighted_match() {
        let mut app = app();
        press(&mut app, ":");
        app.handle_key(key(KeyCode::Down)); // only one palette kind: stays on it
        assert_eq!(app.overlay, Overlay::Palette(String::new(), 0));
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(effects[0], Effect::CancelLoads);
        assert_eq!(app.list.kind.id(), "fake:thing");

        press(&mut app, ":thi");
        assert_eq!(app.overlay, Overlay::Palette("thi".into(), 0));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn a_cached_list_reopens_instantly_and_refreshes() {
        let mut app = app();
        app.handle_key(key(KeyCode::Enter)); // into web-1's parts
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Ok(Page {
                rows: vec![row("p1", "bolt")],
                next: None,
            }),
        });
        app.handle_key(key(KeyCode::Esc));
        let effects = app.handle_key(key(KeyCode::Enter)); // and in again
        assert_eq!(app.list.total(), 1, "shown from the cache straight away");
        assert!(effects.contains(&Effect::LoadList {
            generation: 3,
            from: None
        }));
        // The refresh replaces the cached rows.
        app.handle_msg(Msg::Page {
            generation: 3,
            result: Ok(Page {
                rows: vec![row("p1", "bolt"), row("p2", "nut")],
                next: None,
            }),
        });
        assert_eq!(app.list.total(), 2);
        assert!(app.list.cached_at.is_none());
    }

    #[test]
    fn a_failed_or_unfinished_load_is_not_cached() {
        let mut app = empty_app();
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![row("a", "x")],
                next: Some("t".into()),
            }),
        });
        press(&mut app, ":thing"); // reopen while the first load was still going
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.list.total(), 0);
    }

    #[test]
    fn kinds_that_opt_out_are_served_from_cache_without_a_request() {
        let mut app = app();
        app.registry.register(FakeBilled);
        app.list = ListView::new(Arc::new(FakeBilled), 9);
        app.list.push_page(vec![row("svc", "EC2")], true);
        app.remember_list();
        press(&mut app, ":thing");
        app.handle_key(key(KeyCode::Enter));
        let effects = {
            press(&mut app, ":billed");
            app.handle_key(key(KeyCode::Enter))
        };
        assert_eq!(
            effects,
            [Effect::CancelLoads],
            "no billed request on reopen"
        );
        assert_eq!(app.list.total(), 1);
        // r still refreshes on request.
        assert!(
            press(&mut app, "r")
                .iter()
                .any(|e| matches!(e, Effect::LoadList { .. }))
        );
    }

    /// A kind that shows secrets and opts out of the cache.
    struct FakeSecret;

    #[async_trait::async_trait]
    impl ResourceKind for FakeSecret {
        fn id(&self) -> &'static str {
            "fake:secret"
        }
        fn title(&self) -> &'static str {
            "Secret"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["secret"]
        }
        fn scope(&self) -> Scope {
            Scope::Regional
        }
        fn columns(&self) -> &'static [fast_aws_core::Column] {
            &[]
        }
        fn cacheable(&self) -> bool {
            false
        }
        async fn list(
            &self,
            _: &fast_aws_core::Ctx,
            _: &fast_aws_core::ListQuery,
        ) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(
            &self,
            _: &fast_aws_core::Ctx,
            _: Option<&Parent>,
            _: &str,
        ) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    /// A kind that follows something live, like a log tail.
    struct FakeTail;

    #[async_trait::async_trait]
    impl ResourceKind for FakeTail {
        fn id(&self) -> &'static str {
            "fake:tail"
        }
        fn title(&self) -> &'static str {
            "Tail"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["tail"]
        }
        fn scope(&self) -> Scope {
            Scope::Regional
        }
        fn columns(&self) -> &'static [fast_aws_core::Column] {
            &[]
        }
        fn follow_every(&self) -> Option<Duration> {
            Some(Duration::from_secs(2))
        }
        async fn list(
            &self,
            _: &fast_aws_core::Ctx,
            _: &fast_aws_core::ListQuery,
        ) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(
            &self,
            _: &fast_aws_core::Ctx,
            _: Option<&Parent>,
            _: &str,
        ) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    #[test]
    fn a_followed_list_keeps_loading_and_stays_at_the_end() {
        let mut app = app();
        app.registry.register(FakeTail);
        press(&mut app, ":tail");
        app.handle_key(key(KeyCode::Enter));
        let generation = app.list.generation;
        let page = |rows: Vec<Row>| Msg::Page {
            generation,
            result: Ok(Page {
                rows,
                next: Some("t".into()),
            }),
        };
        app.handle_msg(page(vec![row("e1", "x"), row("e2", "y")]));
        assert!(app.list.is_loading(), "following never finishes");
        assert_eq!(app.list.selected_row().unwrap().id, "e2");
        app.handle_msg(page(vec![row("e3", "z")]));
        assert_eq!(
            app.list.selected_row().unwrap().id,
            "e3",
            "stays at the newest"
        );

        press(&mut app, "k"); // reading an older one (e2): stays put
        app.handle_msg(page(vec![row("e4", "w")]));
        assert_eq!(app.list.selected_row().unwrap().id, "e2");
    }

    #[test]
    fn kinds_that_opt_out_are_never_cached() {
        let mut app = app();
        app.registry.register(FakeSecret);
        press(&mut app, ":secret");
        app.handle_key(key(KeyCode::Enter));
        let generation = app.list.generation;
        app.handle_msg(Msg::Page {
            generation,
            result: Ok(Page {
                rows: vec![row("pw", "hunter2")],
                next: None,
            }),
        });
        press(&mut app, ":thing");
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, ":secret");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.list.total(), 0, "nothing kept after leaving");
    }

    #[test]
    fn profiles_do_not_see_each_others_cache() {
        let mut app = app(); // acme-dev has loaded the list
        press(&mut app, "Pj"); // acme-qa
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.list.total(), 0);
    }

    #[test]
    fn g_jumps_to_a_related_resource_and_esc_comes_back() {
        let mut app = empty_app();
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![
                    row("a", "web-1")
                        .relate("part bolt", "fake:part", "bolt")
                        .relate_inside("parts of web-1", "fake:part", part_parent()),
                ],
                next: None,
            }),
        });
        press(&mut app, "g");
        assert_eq!(app.overlay, Overlay::Relations(0));
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(effects[0], Effect::CancelLoads);
        assert_eq!(app.list.kind.id(), "fake:part");
        assert_eq!(app.list.filter, "bolt");
        assert_eq!(app.list.parent, None);
        assert_eq!(app.breadcrumb(), ["Things", "Parts"]);

        app.handle_key(key(KeyCode::Esc)); // clears the filter first
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.list.kind.id(), "fake:thing");

        press(&mut app, "gj");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.list.parent, Some(part_parent()));
    }

    #[test]
    fn g_also_finds_what_points_here_in_loaded_lists() {
        let mut app = app(); // Things: a (web-1), b (db) are cached
        // A loaded list of parts where p1 relates to thing "db".
        app.handle_key(key(KeyCode::Enter));
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Ok(Page {
                rows: vec![row("p1", "bolt").relate("thing db", "fake:thing", "db")],
                next: None,
            }),
        });
        app.handle_key(key(KeyCode::Esc));
        press(&mut app, "j"); // db
        press(&mut app, "g");
        assert_eq!(app.relations_menu.len(), 1);
        assert_eq!(
            app.relations_menu[0].label,
            "used by bolt (Parts, from loaded lists)"
        );
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.list.kind.id(), "fake:part");
        assert_eq!(
            app.list.parent,
            Some(Parent {
                id: "a".into(),
                label: "a".into(),
                region: None
            })
        );
        assert_eq!(app.list.filter, "p1");
    }

    #[test]
    fn a_row_query_prompts_and_opens_the_answer() {
        let mut app = empty_app();
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![row("a", "web-1").query('S', "find", "parts named", "fake:part", "a")],
                next: None,
            }),
        });
        press(&mut app, "S");
        assert!(
            matches!(&app.overlay, Overlay::Prompt(q, input) if q.key == 'S' && input.is_empty())
        );
        press(&mut app, "bolt");
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(effects[0], Effect::CancelLoads);
        assert_eq!(app.list.kind.id(), "fake:part");
        assert_eq!(app.list.parent.as_ref().unwrap().id, "a\nbolt");
        assert_eq!(app.breadcrumb(), ["Things", "bolt"]);

        // Keys a row doesn't offer do nothing.
        app.handle_key(key(KeyCode::Esc));
        press(&mut app, "Z");
        assert_eq!(app.overlay, Overlay::None);
    }

    pub(crate) fn errors_metric(name: &str) -> fast_aws_core::metrics::MetricSpec {
        fast_aws_core::metrics::MetricSpec {
            namespace: "AWS/Lambda",
            name: "Errors",
            dimensions: vec![("FunctionName", name.to_owned())],
            stat: "Sum",
            summary: fast_aws_core::metrics::Summary::Total,
            unit: "",
            column: 1,
        }
    }

    /// An app whose list has rows with metrics, loaded, with metrics on or off.
    pub(crate) fn app_with_metrics(enabled: bool) -> (App, Vec<Effect>) {
        let mut app = empty_app();
        app.set_metrics(enabled);
        let effects = app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![row("a", "web-1").metric(errors_metric("a")), row("b", "db")],
                next: None,
            }),
        });
        (app, effects)
    }

    #[test]
    fn sparklines_are_requested_once_for_rows_near_the_cursor() {
        let (mut app, effects) = app_with_metrics(true);
        let [Effect::LoadMetrics { scope, metrics }] = effects.as_slice() else {
            panic!("expected one LoadMetrics, got {effects:?}");
        };
        assert_eq!(
            scope,
            &("acme-dev".to_owned(), "eu-west-1".to_owned(), "fake:thing")
        );
        assert_eq!(metrics.len(), 1, "only rows that name a metric");
        assert_eq!(app.sparkline("a"), Some(None), "marked as being fetched");

        // Moving around doesn't ask again while they're fresh.
        assert!(
            press(&mut app, "jk")
                .iter()
                .all(|e| !matches!(e, Effect::LoadMetrics { .. }))
        );

        app.handle_msg(Msg::Metrics {
            scope: scope.clone(),
            result: Ok(HashMap::from([("a".to_owned(), vec![0.0, 2.0])])),
        });
        assert_eq!(app.sparkline("a"), Some(Some(&[0.0, 2.0][..])));
    }

    #[test]
    fn no_sparklines_when_metrics_are_off() {
        let (_, effects) = app_with_metrics(false);
        assert!(effects.is_empty());
    }

    #[test]
    fn a_failed_fetch_says_so_once() {
        let (mut app, effects) = app_with_metrics(true);
        let Effect::LoadMetrics { scope, .. } = &effects[0] else {
            panic!()
        };
        app.handle_msg(Msg::Metrics {
            scope: scope.clone(),
            result: Err(AwsError::new(
                ErrorKind::AccessDenied,
                Some("AccessDenied".into()),
                "no",
            )),
        });
        assert!(
            app.flash
                .as_deref()
                .unwrap()
                .starts_with("sparklines unavailable")
        );
        assert!(
            press(&mut app, "j")
                .iter()
                .all(|e| !matches!(e, Effect::LoadMetrics { .. }))
        );
    }

    #[test]
    fn g_without_relations_explains() {
        let mut app = app();
        press(&mut app, "j"); // db has none
        press(&mut app, "g");
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.flash.as_deref().unwrap().contains("nothing related"));
    }

    #[test]
    fn child_kinds_are_not_in_the_palette() {
        let mut app = app();
        press(&mut app, ":part");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.flash.as_deref(), Some("unknown resource type: part"));
    }

    #[test]
    fn filter_edits_live_and_esc_clears() {
        let mut app = app();
        press(&mut app, "/db");
        assert_eq!(app.overlay, Overlay::Filter);
        assert_eq!(app.list.visible_rows().len(), 1);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.list.filter, "db");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.list.filter, "");
    }

    #[test]
    fn palette_switches_kind_or_reports_unknown() {
        let mut app = app();
        press(&mut app, ":nope");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.flash.as_deref(), Some("unknown resource type: nope"));

        press(&mut app, ":thing");
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadList {
                    generation: 2,
                    from: None
                }
            ]
        );
        // The same kind again: its cached rows show while it reloads.
        assert_eq!(app.list.total(), 2);
        assert!(app.list.is_loading() && app.list.cached_at.is_some());

        press(&mut app, ":q");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Effect::Quit]);
    }

    #[test]
    fn profile_picker_groups_by_session_and_switches() {
        let mut app = app();
        assert_eq!(
            app.profile_entries(),
            [
                PickerEntry::Heading("sso-session acme".into()),
                PickerEntry::Profile("acme-dev".into()),
                PickerEntry::Profile("acme-qa".into()),
                PickerEntry::Heading("other profiles".into()),
                PickerEntry::Profile("ci".into()),
            ]
        );
        press(&mut app, "P");
        assert_eq!(app.overlay, Overlay::Profiles(1));
        press(&mut app, "jj"); // skips the "other profiles" heading
        assert_eq!(app.overlay, Overlay::Profiles(4));
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.profile, "ci");
        assert_eq!(app.region, "eu-west-1"); // ci has no region of its own
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadIdentity { generation: 2 },
                Effect::LoadList {
                    generation: 2,
                    from: None
                }
            ]
        );
        assert_eq!(app.identity, IdentityState::Loading);
    }

    #[test]
    fn region_switch_reloads_the_list_but_not_identity() {
        let mut app = app();
        press(&mut app, "R");
        assert_eq!(app.overlay, Overlay::Regions(6)); // eu-west-1
        press(&mut app, "j");
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.region, "eu-west-2");
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadList {
                    generation: 2,
                    from: None
                }
            ]
        );
    }

    /// A kind billed per request, like Cost Explorer.
    struct FakeBilled;

    #[async_trait::async_trait]
    impl ResourceKind for FakeBilled {
        fn id(&self) -> &'static str {
            "fake:billed"
        }
        fn title(&self) -> &'static str {
            "Billed"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["billed"]
        }
        fn scope(&self) -> Scope {
            Scope::Pinned("us-east-1")
        }
        fn columns(&self) -> &'static [fast_aws_core::Column] {
            &[]
        }
        fn refresh_on_open(&self) -> bool {
            false
        }
        async fn list(
            &self,
            _: &fast_aws_core::Ctx,
            _: &fast_aws_core::ListQuery,
        ) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(
            &self,
            _: &fast_aws_core::Ctx,
            _: Option<&Parent>,
            _: &str,
        ) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    /// A global kind, like IAM.
    struct FakeGlobal;

    #[async_trait::async_trait]
    impl ResourceKind for FakeGlobal {
        fn id(&self) -> &'static str {
            "fake:global"
        }
        fn title(&self) -> &'static str {
            "Globals"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &[]
        }
        fn scope(&self) -> Scope {
            Scope::Global
        }
        fn columns(&self) -> &'static [fast_aws_core::Column] {
            &[]
        }
        async fn list(
            &self,
            _: &fast_aws_core::Ctx,
            _: &fast_aws_core::ListQuery,
        ) -> Result<Page, AwsError> {
            Ok(Page::default())
        }
        async fn describe(
            &self,
            _: &fast_aws_core::Ctx,
            _: Option<&Parent>,
            _: &str,
        ) -> Result<Detail, AwsError> {
            unreachable!()
        }
    }

    #[test]
    fn region_switch_on_a_global_service_does_not_reload() {
        let mut app = app();
        app.list = ListView::new(std::sync::Arc::new(FakeGlobal), 7);
        app.list.push_page(vec![row("r1", "admin")], true);
        press(&mut app, "Rj");
        let effects = app.handle_key(key(KeyCode::Enter));
        assert!(effects.is_empty());
        assert_eq!(app.region, "eu-west-2");
        assert_eq!(app.list.total(), 1);
    }

    #[test]
    fn login_runs_the_session_command_and_reloads_after() {
        let mut app = app();
        app.handle_msg(Msg::Identity {
            generation: 1,
            result: Err(AwsError::new(
                ErrorKind::SessionExpired,
                None,
                "the SSO token has expired",
            )),
        });
        assert!(app.login_would_help());
        let effects = press(&mut app, "L");
        assert_eq!(
            effects,
            [Effect::RunInTerminal(
                ["aws", "sso", "login", "--sso-session", "acme"]
                    .map(String::from)
                    .to_vec()
            )]
        );
        let effects = app.handle_msg(Msg::TerminalCommandFinished { success: true });
        assert_eq!(
            effects,
            [
                Effect::CancelLoads,
                Effect::LoadIdentity { generation: 2 },
                Effect::LoadList {
                    generation: 2,
                    from: None
                }
            ]
        );
    }

    #[test]
    fn a_stale_identity_result_is_ignored_after_a_profile_switch() {
        let mut app = app();
        press(&mut app, "Pjj");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.profile, "ci");
        // The check started for acme-dev finishes late.
        app.handle_msg(Msg::Identity {
            generation: 1,
            result: Ok(CallerIdentity {
                account: "111".into(),
                arn: "arn".into(),
            }),
        });
        assert_eq!(app.identity, IdentityState::Loading);
    }

    #[test]
    fn refresh_and_region_switch_leave_the_identity_check_running() {
        let mut app = app();
        let mut effects = press(&mut app, "r");
        effects.extend(press(&mut app, "Rj"));
        effects.extend(app.handle_key(key(KeyCode::Enter)));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadIdentity { .. }))
        );
        app.handle_msg(Msg::Identity {
            generation: 1,
            result: Ok(CallerIdentity {
                account: "111".into(),
                arn: "arn".into(),
            }),
        });
        assert!(matches!(app.identity, IdentityState::Ready(_)));
    }

    fn safety_config() -> Config {
        toml::from_str("[profiles.ci]\nread_only = true\n[profiles.acme-qa]\nprod = true\n")
            .unwrap()
    }

    #[test]
    fn the_guard_follows_the_profile() {
        let mut app = app();
        app.set_safety(safety_config(), false);
        assert!(!app.guard.read_only && !app.guard.prod);
        press(&mut app, "Pjj"); // ci
        app.handle_key(key(KeyCode::Enter));
        assert!(app.guard.read_only);
        let mut app = super::tests::app();
        app.set_safety(safety_config(), true);
        assert!(app.guard.read_only, "--read-only applies to every profile");
    }

    #[test]
    fn unlock_only_applies_to_prod_and_ends_with_the_profile() {
        let mut app = app();
        app.set_safety(safety_config(), false);
        press(&mut app, ":unlock");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.flash.as_deref().unwrap().contains("nothing to unlock"));

        press(&mut app, "Pj"); // acme-qa, marked prod
        app.handle_key(key(KeyCode::Enter));
        assert!(app.guard.prod && !app.guard.unlocked);
        press(&mut app, ":unlock");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.guard.unlocked);

        press(&mut app, "Pk"); // back to acme-dev, then to acme-qa again
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "Pj");
        app.handle_key(key(KeyCode::Enter));
        assert!(
            !app.guard.unlocked,
            "unlocking lasts only while on that profile"
        );
    }

    #[test]
    fn login_on_a_non_sso_profile_explains() {
        let mut app = app();
        app.profile = "ci".into();
        assert!(press(&mut app, "L").is_empty());
        assert!(app.flash.as_deref().unwrap().contains("doesn't use SSO"));
    }

    #[test]
    fn list_errors_show_after_identity_succeeds() {
        let mut app = app();
        press(&mut app, "r");
        let denied = AwsError::new(
            ErrorKind::AccessDenied,
            Some("UnauthorizedOperation".into()),
            "no",
        );
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Err(denied.clone()),
        });
        assert_eq!(app.list.status, LoadStatus::Failed(denied.clone()));
        assert_eq!(app.blocking_error(), Some(&denied));
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = app();
        assert_eq!(press(&mut app, "q"), [Effect::Quit]);
        let mut app = super::tests::app();
        let effects = app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(effects, [Effect::Quit]);
    }
}
