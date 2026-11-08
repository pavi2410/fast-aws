//! Mouse input: click, double-click, hover, wheel and right-click. The renderer
//! records where clickable things were drawn ([`Hits`]); this maps pointer
//! positions back onto them and turns clicks into the key presses they stand for.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use super::{App, Effect, Overlay, PickerEntry, REGIONS, SETTINGS, Screen};

/// Two clicks on the same row within this open it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows the wheel moves a list or a detail view per notch.
const WHEEL_STEP: isize = 3;

/// Where the last frame drew things the pointer can act on.
#[derive(Clone, Debug, Default)]
pub struct Hits {
    /// The title line: a click goes back.
    pub title: Rect,
    /// Table rows (below the column header).
    pub table: Rect,
    /// First visible row index in the table.
    pub table_offset: usize,
    /// The selected row's index in the table, if any.
    pub table_selected: Option<usize>,
    /// The open popup, if any.
    pub popup: Option<Rect>,
    /// Item lines inside the popup, one item per line.
    pub list: Rect,
    /// Index of the item on the popup's first line.
    pub list_offset: usize,
    /// Header and footer labels, with the key a click on each presses.
    pub zones: Vec<(Rect, KeyCode)>,
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

impl App {
    /// The table row (index into the visible rows) at `pos`, when no popup is open.
    pub fn table_target(&self, pos: Position) -> Option<usize> {
        let h = self.hits.borrow();
        if h.popup.is_some() || !h.table.contains(pos) {
            return None;
        }
        let index = h.table_offset + usize::from(pos.y - h.table.y);
        (index < self.list.visible_rows().len()).then_some(index)
    }

    /// The popup item at `pos`, skipping headings and empty lines.
    pub fn overlay_target(&self, pos: Position) -> Option<usize> {
        let h = self.hits.borrow();
        if h.popup.is_none() || !h.list.contains(pos) {
            return None;
        }
        let index = h.list_offset + usize::from(pos.y - h.list.y);
        let valid = match &self.overlay {
            Overlay::Palette(input, _) => index < self.registry.search(input).len(),
            Overlay::Profiles(_) => matches!(
                self.profile_entries().get(index),
                Some(PickerEntry::Profile(_))
            ),
            Overlay::Regions(_) => index < REGIONS.len(),
            Overlay::Relations(_) => index < self.relations_menu.len(),
            Overlay::Settings(_) => index < SETTINGS.len(),
            _ => false,
        };
        valid.then_some(index)
    }

    /// The popup item the cursor is on, for hover to skip.
    pub fn overlay_cursor(&self) -> Option<usize> {
        match self.overlay {
            Overlay::Palette(_, at)
            | Overlay::Profiles(at)
            | Overlay::Regions(at)
            | Overlay::Relations(at)
            | Overlay::Settings(at) => Some(at),
            _ => None,
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> Vec<Effect> {
        let mut effects = self.handle_mouse_inner(event);
        effects.extend(self.sparklines_effect());
        effects
    }

    fn handle_mouse_inner(&mut self, event: MouseEvent) -> Vec<Effect> {
        let pos = Position::new(event.column, event.row);
        self.mouse_pos = Some((pos.x, pos.y));
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(pos),
            MouseEventKind::Down(MouseButton::Right) => self.handle_key_inner(press(KeyCode::Esc)),
            MouseEventKind::ScrollDown => self.wheel(1),
            MouseEventKind::ScrollUp => self.wheel(-1),
            _ => Vec::new(),
        }
    }

    fn wheel(&mut self, direction: isize) -> Vec<Effect> {
        match self.overlay {
            Overlay::None | Overlay::Filter => match &mut self.screen {
                Screen::List => self.list.move_by(direction * WHEEL_STEP),
                Screen::Detail(d) => d.scroll_by((direction * WHEEL_STEP) as i32),
            },
            Overlay::Palette(..)
            | Overlay::Profiles(_)
            | Overlay::Regions(_)
            | Overlay::Relations(_)
            | Overlay::Settings(_) => {
                let key = if direction > 0 {
                    KeyCode::Down
                } else {
                    KeyCode::Up
                };
                return self.handle_key_inner(press(key));
            }
            Overlay::Prompt(..) | Overlay::Help => {}
        }
        Vec::new()
    }

    fn click(&mut self, pos: Position) -> Vec<Effect> {
        self.flash = None;
        match self.overlay.clone() {
            Overlay::Help => {
                self.overlay = Overlay::None;
                Vec::new()
            }
            Overlay::Prompt(..) => Vec::new(),
            Overlay::None | Overlay::Filter => {
                if self.overlay == Overlay::Filter {
                    // Clicking away keeps what was typed, like Enter.
                    self.overlay = Overlay::None;
                }
                self.click_screen(pos)
            }
            overlay => self.click_popup(overlay, pos),
        }
    }

    fn click_popup(&mut self, overlay: Overlay, pos: Position) -> Vec<Effect> {
        let popup = self.hits.borrow().popup;
        if let Some(index) = self.overlay_target(pos) {
            self.overlay = match overlay {
                Overlay::Palette(input, _) => Overlay::Palette(input, index),
                Overlay::Profiles(_) => Overlay::Profiles(index),
                Overlay::Regions(_) => Overlay::Regions(index),
                Overlay::Relations(_) => Overlay::Relations(index),
                Overlay::Settings(_) => Overlay::Settings(index),
                other => other,
            };
            // A click on an item does what Enter on it does.
            return self.handle_key_inner(press(KeyCode::Enter));
        }
        if popup.is_some_and(|p| !p.contains(pos)) {
            self.overlay = Overlay::None;
        }
        Vec::new()
    }

    fn click_screen(&mut self, pos: Position) -> Vec<Effect> {
        let (zone, title) = {
            let h = self.hits.borrow();
            let zone = h
                .zones
                .iter()
                .find(|(r, _)| r.contains(pos))
                .map(|(_, k)| *k);
            (zone, h.title.contains(pos))
        };
        if let Some(code) = zone {
            return self.handle_key_inner(press(code));
        }
        if title {
            return match self.screen {
                Screen::Detail(_) => {
                    self.screen = Screen::List;
                    Vec::new()
                }
                Screen::List if !self.back.is_empty() => self.go_back(),
                Screen::List => Vec::new(),
            };
        }
        let Some(index) = self.table_target(pos) else {
            return Vec::new();
        };
        if !matches!(self.screen, Screen::List) {
            return Vec::new();
        }
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(at, row)| row == index && now.duration_since(at) < DOUBLE_CLICK);
        self.list.select(index);
        if double {
            self.last_click = None;
            return self.open_selected();
        }
        self.last_click = Some((now, index));
        Vec::new()
    }
}
