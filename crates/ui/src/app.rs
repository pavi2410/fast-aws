//! The app state machine. Key presses and background results go in; state changes
//! and [`Effect`]s (work for the runtime to start) come out. Nothing here touches
//! the terminal or AWS, so all of it is unit-tested.

use std::sync::Arc;

use fast_aws_core::profiles::{ProfileCatalog, ProfileInfo};
use fast_aws_core::{AwsError, CallerIdentity, Detail, ErrorKind, Page, Registry, ResourceKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::detail::{DetailState, DetailView};
use crate::list::ListView;

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
    Quit,
}

/// Results coming back from the runtime.
#[derive(Debug)]
pub enum Msg {
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
    /// `:` command palette, with what has been typed.
    Palette(String),
    /// `/` filter being edited (the text lives in the list).
    Filter,
    /// `P`: index into [`App::profile_entries`].
    Profiles(usize),
    /// `R`: index into [`REGIONS`].
    Regions(usize),
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
                        let last = page.next.is_none();
                        self.list.push_page(page.rows, last);
                    }
                    Err(e) => self.list.fail(e),
                }
            }
            Msg::Paused { generation, next } if generation == self.list.generation => {
                self.list.pause(next);
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

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![Effect::Quit];
        }
        self.flash = None;
        match self.overlay.clone() {
            Overlay::Palette(input) => self.palette_key(key, input),
            Overlay::Filter => self.filter_key(key),
            Overlay::Profiles(i) => self.profiles_key(key, i),
            Overlay::Regions(i) => self.regions_key(key, i),
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
                self.overlay = Overlay::Palette(String::new());
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
        let generation = self.generation();
        let child = ListView::inside(kind, Some(drill.parent), generation);
        self.back.push(std::mem::replace(&mut self.list, child));
        vec![
            Effect::CancelLoads,
            Effect::LoadList {
                generation,
                from: None,
            },
        ]
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
    pub fn breadcrumb(&self) -> Vec<&str> {
        let lists = self.back.iter().chain(std::iter::once(&self.list));
        let mut crumbs = vec![self.back.first().unwrap_or(&self.list).kind.title()];
        crumbs.extend(lists.filter_map(|l| l.parent.as_ref().map(|p| p.label.as_str())));
        crumbs
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

    fn palette_key(&mut self, key: KeyEvent, mut input: String) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                return self.run_command(&input);
            }
            KeyCode::Backspace => {
                input.pop();
                self.overlay = Overlay::Palette(input);
            }
            KeyCode::Char(c) => {
                input.push(c);
                self.overlay = Overlay::Palette(input);
            }
            _ => {}
        }
        Vec::new()
    }

    fn run_command(&mut self, input: &str) -> Vec<Effect> {
        let command = input.trim();
        if matches!(command, "q" | "quit") {
            self.should_quit = true;
            return vec![Effect::Quit];
        }
        if command.is_empty() {
            return Vec::new();
        }
        let Some(kind) = self.registry.find(command) else {
            self.flash = Some(format!("unknown resource type: {command}"));
            return Vec::new();
        };
        let generation = self.generation();
        self.list = ListView::new(kind, generation);
        self.back.clear();
        self.screen = Screen::List;
        vec![
            Effect::CancelLoads,
            Effect::LoadList {
                generation,
                from: None,
            },
        ]
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
        let generation = self.generation();
        let root = self.back.first().unwrap_or(&self.list).kind.clone();
        self.list = ListView::new(root, generation);
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
        effects.push(Effect::LoadList {
            generation,
            from: None,
        });
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
    use fast_aws_core::Parent;
    use fast_aws_core::profiles::Auth;

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
        assert_eq!(app.list.total(), 0);

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
