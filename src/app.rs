//! Application state and key handling. Nothing here draws or reads the
//! terminal, so all of it is covered by plain unit tests.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::config::Config;
use crate::format;
use crate::metrics::{Control, HostInfo, Snapshot};
use crate::panel::{PanelId, PanelSet};
use crate::process::{self, KillReport, KillRequest, KillSignal, KillTarget, ProcRow, ProcView};

pub use crate::process::SortKey;

pub const HISTORY_LEN: usize = 120;
pub const MIN_INTERVAL_MS: u64 = 250;
pub const MAX_INTERVAL_MS: u64 = 8_000;
const PAGE: u16 = 10;

/// Everything the UI thread reacts to. The input and sampler threads both send into one channel.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one Metrics event per tick is moved, never copied; a Box would add an allocation"
)]
pub enum Event {
    Key(KeyEvent),
    /// Text pasted while the terminal has bracketed paste on.
    Paste(String),
    Resize,
    Metrics(Snapshot),
    KillDone(KillReport),
}

/// A fixed-size ring buffer. When it is full, a new sample pushes out the oldest one.
#[derive(Debug)]
pub struct History<T> {
    samples: VecDeque<T>,
    cap: usize,
}

impl<T: Copy> History<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, value: T) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = T> + '_ {
        self.samples.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    /// The prompt keeps its target, so a re-sort cannot change which process gets the signal.
    ConfirmKill(KillTarget),
}

pub struct App {
    pub host: HostInfo,
    pub snapshot: Option<Snapshot>,
    pub cpu_history: History<f64>,
    pub mem_history: History<f64>,
    pub rx_history: History<u64>,
    pub tx_history: History<u64>,
    pub sort: SortKey,
    pub filter: String,
    pub mode: Mode,
    pub paused: bool,
    pub table: TableState,
    pub should_quit: bool,
    pub reverse: bool,
    pub tree: bool,
    pub show_cmd: bool,
    pub net_detail: bool,
    pub fahrenheit: bool,
    /// Panels the user wants. Layout can still collapse some.
    pub visible: PanelSet,
    /// One footer message. The next key press clears it.
    pub status: Option<String>,
    /// The process row that the last frame highlighted. `ui::draw` sets it on each frame.
    /// The `x` key builds the kill target from this, not from `table.selected()`.
    pub highlighted: Option<KillTarget>,
    /// True once a frame has shown the kill prompt. `ui::draw` sets it; `x` clears it.
    pub kill_prompt_drawn: bool,
    /// To the sampler thread: kills, wake-ups and pause.
    control_tx: Option<Sender<Control>>,
    interval_ms: Arc<AtomicU64>,
}

impl App {
    pub fn new(host: HostInfo, interval_ms: Arc<AtomicU64>) -> Self {
        Self {
            host,
            snapshot: None,
            cpu_history: History::new(HISTORY_LEN),
            mem_history: History::new(HISTORY_LEN),
            rx_history: History::new(HISTORY_LEN),
            tx_history: History::new(HISTORY_LEN),
            sort: SortKey::Cpu,
            filter: String::new(),
            mode: Mode::Normal,
            paused: false,
            table: TableState::default().with_selected(0),
            should_quit: false,
            reverse: false,
            tree: false,
            show_cmd: false,
            net_detail: false,
            fahrenheit: false,
            visible: PanelSet::ALL,
            status: None,
            highlighted: None,
            kill_prompt_drawn: false,
            control_tx: None,
            interval_ms,
        }
    }

    /// The channel to the sampler thread, which sends the signals.
    pub fn with_control_sender(mut self, tx: Sender<Control>) -> Self {
        self.control_tx = Some(tx);
        self
    }

    /// Sends one message to the sampler. False when there is no sampler to take it.
    fn send(&self, message: Control) -> bool {
        self.control_tx
            .as_ref()
            .is_some_and(|tx| tx.send(message).is_ok())
    }

    /// Applies config values, then the caller applies CLI overrides.
    pub fn apply_config(&mut self, config: &Config) {
        if let Some(ms) = config.interval_ms {
            self.interval_ms.store(
                ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS),
                Ordering::Relaxed,
            );
        }
        if let Some(sort) = config.sort.as_deref().and_then(SortKey::from_name) {
            self.sort = sort;
        }
        let flags = [
            (config.reverse, &mut self.reverse),
            (config.tree, &mut self.tree),
            (config.command, &mut self.show_cmd),
            (config.fahrenheit, &mut self.fahrenheit),
            (config.net_detail, &mut self.net_detail),
        ];
        for (value, field) in flags {
            if let Some(value) = value {
                *field = value;
            }
        }
        if let Some(panels) = config.panels {
            self.visible = panels;
        }
        if let Some((first, rest)) = config.warnings.split_first() {
            self.status = Some(match rest.len() {
                0 => first.clone(),
                more => format!("{first} (+{more} more)"),
            });
        }
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms.load(Ordering::Relaxed)
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Paste(text) => self.on_paste(&text),
            Event::Metrics(snapshot) => self.on_metrics(snapshot),
            // The result of a kill matters even while the view is paused.
            Event::KillDone(report) => self.status = Some(report.message()),
            Event::Resize => {} // The next draw picks up the new size.
        }
    }

    fn on_metrics(&mut self, snapshot: Snapshot) {
        if self.paused {
            return;
        }
        self.cpu_history.push(f64::from(snapshot.cpu_total));
        self.mem_history
            .push(format::percent(snapshot.mem_used, snapshot.mem_total));
        self.rx_history.push(snapshot.rx_rate);
        self.tx_history.push(snapshot.tx_rate);
        self.snapshot = Some(snapshot);
    }

    /// Only the filter takes pasted text. A paste never confirms a kill: it cancels it.
    fn on_paste(&mut self, text: &str) {
        self.status = None;
        match self.mode {
            Mode::Filter => {
                self.filter.extend(text.chars().filter(|c| !c.is_control()));
                self.table.select_first();
            }
            Mode::ConfirmKill(_) => self.cancel_kill(),
            Mode::Normal | Mode::Help => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.status = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match &self.mode {
            Mode::Filter => self.on_filter_key(key.code),
            Mode::Normal => {
                // Shift is part of the character (`G`, `?`); Ctrl and Alt are not rtop keys.
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    self.on_normal_key(key.code);
                }
            }
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::F(1)
                ) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::ConfirmKill(target) => {
                let target = target.clone();
                self.on_confirm_key(key, target);
            }
        }
    }

    fn on_filter_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => return,
        }
        self.table.select_first();
    }

    fn on_normal_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('s') => self.sort = self.sort.next(),
            KeyCode::Char('r') => self.reverse = !self.reverse,
            KeyCode::Char('t') => self.tree = !self.tree,
            KeyCode::Char('c') => self.show_cmd = !self.show_cmd,
            KeyCode::Char('n') => self.net_detail = !self.net_detail,
            KeyCode::Char('x') | KeyCode::F(9) => self.ask_kill(),
            KeyCode::Char('0') => self.visible = PanelSet::ALL,
            KeyCode::Char(c @ '1'..='8') => {
                if let Some(id) = PanelId::from_key(c) {
                    self.toggle_panel(id);
                }
            }
            KeyCode::Char(' ') => {
                self.paused = !self.paused;
                self.send(Control::Pause(self.paused));
            }
            KeyCode::Char('+' | '=') => self.scale_interval(2.0),
            KeyCode::Char('-') => self.scale_interval(0.5),
            KeyCode::Down | KeyCode::Char('j') => self.table.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.table.select_previous(),
            KeyCode::PageDown => self.table.scroll_down_by(PAGE),
            KeyCode::PageUp => self.table.scroll_up_by(PAGE),
            KeyCode::Home | KeyCode::Char('g') => self.table.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.table.select_last(),
            _ => {}
        }
    }

    /// `y` and `f` send, `n` and `Esc` cancel. Every other key, `Enter` too, cancels with
    /// a message, so keys typed or pasted into the wrong window cannot reach `y` or `f`.
    /// `y` and `f` also cancel when no frame has shown the prompt yet: `main.rs` handles a
    /// batch of keys before it redraws, so they were not an answer to the prompt.
    fn on_confirm_key(&mut self, key: KeyEvent, target: KillTarget) {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let signal = match key.code {
            KeyCode::Char('y') if plain && self.kill_prompt_drawn => KillSignal::Term,
            KeyCode::Char('f') if plain && self.kill_prompt_drawn => KillSignal::Kill,
            KeyCode::Char('n') | KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            _ => {
                self.cancel_kill();
                return;
            }
        };
        self.mode = Mode::Normal;
        let message = format!(
            "sending {} to {} {}…",
            signal.label(),
            target.pid,
            target.name
        );
        let sent = self.send(Control::Kill(KillRequest { target, signal }));
        self.status = Some(if sent {
            message
        } else {
            "kill is not available".to_owned()
        });
    }

    fn cancel_kill(&mut self) {
        self.mode = Mode::Normal;
        self.status = Some("kill cancelled".to_owned());
    }

    /// Opens the kill prompt for the row that the last frame highlighted.
    fn ask_kill(&mut self) {
        let Some(target) = self.highlighted.clone() else {
            self.status = Some("nothing selected".to_owned());
            return;
        };
        if let Err(error) = process::check_target(target.pid, std::process::id()) {
            let report = KillReport {
                request: KillRequest {
                    target,
                    signal: KillSignal::Term,
                },
                result: Err(error),
            };
            self.status = Some(report.message());
            return;
        }
        self.kill_prompt_drawn = false;
        self.mode = Mode::ConfirmKill(target);
    }

    fn toggle_panel(&mut self, id: PanelId) {
        match &self.snapshot {
            Some(snapshot) if !has_data(snapshot, id) => {
                self.status = Some(format!("{}: not available here", id.name()));
            }
            _ => self.visible.toggle(id),
        }
    }

    /// Stores the new interval before it wakes the sampler, so the sampler reads the new value.
    fn scale_interval(&mut self, factor: f64) {
        let next = (self.interval_ms() as f64 * factor) as u64;
        self.interval_ms.store(
            next.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS),
            Ordering::Relaxed,
        );
        self.send(Control::Wake);
    }

    /// Processes that match the filter, in the current sort order, as the table shows them.
    pub fn visible_rows(&self) -> Vec<ProcRow<'_>> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let view = ProcView {
            filter: &self.filter,
            sort: self.sort,
            reverse: self.reverse,
            tree: self.tree,
            // When the table shows command lines, the filter finds text in them too.
            match_cmd: self.show_cmd,
        };
        process::visible_rows(&snapshot.processes, &view)
    }
}

/// True when this host gave data for the panel. A panel without data takes no space.
pub fn has_data(snapshot: &Snapshot, id: PanelId) -> bool {
    match id {
        PanelId::Storage => !snapshot.storage.volumes.is_empty(),
        PanelId::Zfs => snapshot.zfs.is_some(),
        PanelId::Sensors => !snapshot.sensors.is_empty(),
        PanelId::Cpu
        | PanelId::History
        | PanelId::Memory
        | PanelId::Network
        | PanelId::Processes => true,
    }
}

/// Keeps the selection inside a list of `len` rows, for example after the filter shrinks it.
pub fn clamp_selection(table: &mut TableState, len: usize) {
    let selected = match len {
        0 => None,
        _ => Some(table.selected().unwrap_or(0).min(len - 1)),
    };
    table.select(selected);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{KillError, ProcInfo};
    use crate::sensors::{Sensors, Temp};
    use crate::storage::{Storage, Volume};
    use crate::zfs::ZfsArc;
    use std::sync::mpsc::{self, Receiver};

    fn proc(pid: u32, name: &str, cpu: f32, memory: u64) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cpu,
            memory,
            ident: 1_000 + u64::from(pid),
            ..ProcInfo::default()
        }
    }

    fn app_with(processes: Vec<ProcInfo>) -> App {
        let mut app = App::new(HostInfo::default(), Arc::new(AtomicU64::new(1_000)));
        app.handle(Event::Metrics(Snapshot {
            processes,
            mem_total: 100,
            mem_used: 25,
            ..Snapshot::default()
        }));
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle(Event::Key(KeyEvent::from(code)));
    }

    fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(Event::Key(KeyEvent::new(code, modifiers)));
    }

    fn pids(app: &App) -> Vec<u32> {
        app.visible_rows().iter().map(|row| row.proc.pid).collect()
    }

    /// Draws one frame, so `highlighted` follows the selected row, as in the real loop.
    fn draw(app: &mut App) {
        crate::ui::render_to_string(110, 40, |frame| crate::ui::draw(frame, app));
    }

    fn target(pid: u32, name: &str) -> KillTarget {
        KillTarget {
            pid,
            ident: 1_000 + u64::from(pid),
            name: name.into(),
        }
    }

    fn app_with_kill_channel(processes: Vec<ProcInfo>) -> (App, Receiver<Control>) {
        let (tx, rx) = mpsc::channel();
        let app = app_with(processes).with_control_sender(tx);
        (app, rx)
    }

    /// True when no kill request is waiting in `rx`. Other messages are drained too.
    fn no_kill(rx: &Receiver<Control>) -> bool {
        rx.try_iter().all(|c| !matches!(c, Control::Kill(_)))
    }

    #[test]
    fn history_drops_the_oldest_sample_when_full() {
        let mut h = History::new(3);
        for v in 1..=5 {
            h.push(v);
        }
        assert_eq!(h.iter().collect::<Vec<_>>(), [3, 4, 5]);
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn sorts_by_each_column() {
        let mut app = app_with(vec![
            ProcInfo {
                read_rate: 7,
                ..proc(30, "zsh", 5.0, 10)
            },
            proc(10, "Brave", 50.0, 5),
            ProcInfo {
                write_rate: 9,
                ..proc(20, "cargo", 5.0, 99)
            },
        ]);
        assert_eq!(pids(&app), [10, 20, 30], "cpu desc, ties by pid");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(pids(&app), [20, 30, 10], "memory desc");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Io);
        assert_eq!(pids(&app), [20, 30, 10], "io desc");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(pids(&app), [10, 20, 30], "pid asc");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(pids(&app), [10, 20, 30], "name asc, case-insensitive");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Cpu, "cycle wraps");
    }

    #[test]
    fn filter_mode_edits_and_clears() {
        let mut app = app_with(vec![
            proc(1, "launchd", 0.0, 1),
            proc(42, "Cargo", 0.0, 1),
            proc(7, "rust-analyzer", 0.0, 1),
        ]);
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.mode, Mode::Filter);
        for c in "CAR".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(pids(&app), [42], "match is case-insensitive");
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.should_quit, "q is text while filtering");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filter, "CAR", "backspace removed the q");
        press(&mut app, KeyCode::Esc);
        assert_eq!(pids(&app).len(), 3, "esc clears the filter");
    }

    #[test]
    fn filter_matches_an_exact_pid() {
        let mut app = app_with(vec![proc(42, "a", 0.0, 1), proc(420, "b", 0.0, 1)]);
        app.filter = "42".into();
        assert_eq!(pids(&app), [42]);
    }

    #[test]
    fn interval_doubles_and_halves_within_bounds() {
        let mut app = app_with(vec![]);
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(app.interval_ms(), 2_000);
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('-'));
        }
        assert_eq!(app.interval_ms(), MIN_INTERVAL_MS);
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('+'));
        }
        assert_eq!(app.interval_ms(), MAX_INTERVAL_MS);
    }

    #[test]
    fn interval_keys_store_the_interval_then_wake_the_sampler() {
        let (mut app, rx) = app_with_kill_channel(vec![]);
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(app.interval_ms(), 2_000, "stored before the wake-up");
        assert_eq!(rx.try_recv(), Ok(Control::Wake));
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(app.interval_ms(), 1_000);
        assert_eq!(rx.try_recv(), Ok(Control::Wake));
        press(&mut app, KeyCode::Char('='));
        assert_eq!(rx.try_recv(), Ok(Control::Wake), "= is + without shift");
        assert!(rx.try_recv().is_err(), "one wake-up for each key");
    }

    #[test]
    fn space_pauses_and_resumes_the_sampler() {
        let (mut app, rx) = app_with_kill_channel(vec![]);
        press(&mut app, KeyCode::Char(' '));
        assert!(app.paused);
        assert_eq!(rx.try_recv(), Ok(Control::Pause(true)));
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.paused);
        assert_eq!(rx.try_recv(), Ok(Control::Pause(false)));
    }

    #[test]
    fn pause_freezes_the_snapshot_and_history() {
        let mut app = app_with(vec![proc(1, "a", 0.0, 1)]);
        press(&mut app, KeyCode::Char(' '));
        app.handle(Event::Metrics(Snapshot::default()));
        assert_eq!(app.cpu_history.len(), 1);
        assert_eq!(pids(&app), [1]);
        assert_eq!(app.mem_history.iter().next(), Some(25.0));
    }

    #[test]
    fn selection_is_clamped_to_the_visible_rows() {
        let mut table = TableState::default().with_selected(50);
        clamp_selection(&mut table, 3);
        assert_eq!(table.selected(), Some(2));
        clamp_selection(&mut table, 0);
        assert_eq!(table.selected(), None);
        clamp_selection(&mut table, 4);
        assert_eq!(table.selected(), Some(0), "rows came back: select the top");
    }

    #[test]
    fn ctrl_c_quits_in_any_mode() {
        for setup in [
            KeyCode::Char('/'),
            KeyCode::Char('?'),
            KeyCode::Char('x'),
            KeyCode::Null,
        ] {
            let mut app = app_with(vec![proc(500, "a", 0.0, 1)]);
            draw(&mut app);
            press(&mut app, setup);
            press_with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(app.should_quit, "after {setup:?}");
        }
    }

    #[test]
    fn x_asks_to_kill_the_highlighted_row() {
        let mut app = app_with(vec![proc(500, "sleep", 9.0, 1), proc(600, "zsh", 1.0, 1)]);
        draw(&mut app);
        press(&mut app, KeyCode::Down);
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.mode, Mode::ConfirmKill(target(600, "zsh")));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, KeyCode::F(9));
        assert_eq!(app.mode, Mode::ConfirmKill(target(600, "zsh")), "F9 too");
    }

    #[test]
    fn kill_target_is_the_row_on_screen_not_the_resorted_row() {
        let mut app = app_with(vec![proc(500, "a", 9.0, 1), proc(600, "b", 1.0, 1)]);
        draw(&mut app);
        assert_eq!(app.highlighted, Some(target(500, "a")));
        // One batch: new metrics sort b first, then `x`, with no draw between.
        app.handle(Event::Metrics(Snapshot {
            processes: vec![proc(500, "a", 1.0, 1), proc(600, "b", 9.0, 1)],
            ..Snapshot::default()
        }));
        assert_eq!(pids(&app), [600, 500], "the table index now points at b");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.mode, Mode::ConfirmKill(target(500, "a")));
    }

    #[test]
    fn x_with_nothing_highlighted_says_so() {
        let mut app = app_with(vec![]);
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status.as_deref(), Some("nothing selected"));
    }

    #[test]
    fn y_sends_term_and_f_sends_kill() {
        for (key, signal) in [('y', KillSignal::Term), ('f', KillSignal::Kill)] {
            let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
            draw(&mut app);
            press(&mut app, KeyCode::Char('x'));
            draw(&mut app);
            press(&mut app, KeyCode::Char(key));
            assert_eq!(
                rx.try_recv(),
                Ok(Control::Kill(KillRequest {
                    target: target(500, "sleep"),
                    signal
                }))
            );
            assert_eq!(app.mode, Mode::Normal);
            assert_eq!(
                app.status,
                Some(format!("sending {} to 500 sleep…", signal.label()))
            );
        }
    }

    #[test]
    fn n_and_esc_cancel_without_a_message() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
            draw(&mut app);
            press(&mut app, KeyCode::Char('x'));
            draw(&mut app);
            press(&mut app, key);
            assert_eq!(app.mode, Mode::Normal, "{key:?} cancels");
            assert_eq!(app.status, None, "{key:?}");
            assert!(rx.try_recv().is_err(), "{key:?} sends nothing");
        }
    }

    #[test]
    fn any_other_key_cancels_the_kill_prompt() {
        for key in [
            KeyCode::Enter,
            KeyCode::Char('q'),
            KeyCode::Char('x'),
            KeyCode::Char(' '),
            KeyCode::Char('|'),
            KeyCode::Char('Y'),
            KeyCode::Down,
            KeyCode::F(9),
        ] {
            let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
            draw(&mut app);
            press(&mut app, KeyCode::Char('x'));
            draw(&mut app);
            press(&mut app, key);
            assert_eq!(app.mode, Mode::Normal, "{key:?} cancels");
            assert_eq!(app.status.as_deref(), Some("kill cancelled"), "{key:?}");
            assert!(!app.should_quit, "{key:?} does not quit from the prompt");
            assert!(rx.try_recv().is_err(), "{key:?} sends nothing");
        }
    }

    #[test]
    fn ctrl_or_alt_with_y_or_f_cancels() {
        for modifiers in [
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            for key in ['y', 'f'] {
                let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
                draw(&mut app);
                press(&mut app, KeyCode::Char('x'));
                draw(&mut app);
                press_with(&mut app, KeyCode::Char(key), modifiers);
                assert_eq!(app.mode, Mode::Normal, "{modifiers:?}+{key}");
                assert!(rx.try_recv().is_err(), "{modifiers:?}+{key} sends nothing");
            }
        }
    }

    #[test]
    fn y_before_the_prompt_is_drawn_cancels() {
        // main.rs handles a whole batch of keys before it redraws, so `x` then `y` in one
        // batch would confirm a prompt that the user never saw.
        for key in ['y', 'f'] {
            let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
            draw(&mut app);
            press(&mut app, KeyCode::Char('x'));
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.mode, Mode::Normal, "{key}");
            assert_eq!(app.status.as_deref(), Some("kill cancelled"), "{key}");
            assert!(rx.try_recv().is_err(), "{key} sends nothing");
        }
    }

    #[test]
    fn typed_shell_command_sends_no_signal() {
        // A paste without bracketed paste arrives as one key per character, in one batch.
        let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
        draw(&mut app);
        for c in "ps aux | grep firefox".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        draw(&mut app);
        for c in "xy".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        draw(&mut app);
        for c in " firefox".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(no_kill(&rx), "no signal");
    }

    #[test]
    fn paste_goes_into_the_filter_only() {
        let mut app = app_with(vec![proc(500, "sleep", 0.0, 1), proc(600, "zsh", 0.0, 1)]);
        app.handle(Event::Paste("zsh".into()));
        assert_eq!(app.filter, "", "a paste in Normal mode does nothing");
        press(&mut app, KeyCode::Char('/'));
        app.handle(Event::Paste("zs\nh\t".into()));
        assert_eq!(app.filter, "zsh", "control characters are dropped");
        assert_eq!(pids(&app), [600]);
    }

    #[test]
    fn paste_cancels_the_kill_prompt() {
        let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        draw(&mut app);
        app.handle(Event::Paste("y".into()));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status.as_deref(), Some("kill cancelled"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn x_on_pid_1_is_refused() {
        let mut app = app_with(vec![proc(1, "init", 0.0, 1)]);
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status.as_deref(), Some("refused: pid 1 is protected"));
    }

    #[test]
    fn kill_without_a_sender_is_not_available() {
        let mut app = app_with(vec![proc(500, "sleep", 0.0, 1)]);
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        draw(&mut app);
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.status.as_deref(), Some("kill is not available"));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn kill_with_a_closed_channel_is_not_available() {
        let (mut app, rx) = app_with_kill_channel(vec![proc(500, "sleep", 0.0, 1)]);
        drop(rx);
        draw(&mut app);
        press(&mut app, KeyCode::Char('x'));
        draw(&mut app);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.status.as_deref(), Some("kill is not available"));
    }

    #[test]
    fn kill_done_shows_its_message_even_when_paused() {
        let mut app = app_with(vec![]);
        press(&mut app, KeyCode::Char(' '));
        app.handle(Event::KillDone(KillReport {
            request: KillRequest {
                target: target(500, "sleep"),
                signal: KillSignal::Term,
            },
            result: Err(KillError::Gone),
        }));
        assert_eq!(app.status.as_deref(), Some("500 sleep is not running now"));
    }

    #[test]
    fn help_opens_and_closes() {
        for close in [
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Char('?'),
            KeyCode::F(1),
        ] {
            let mut app = app_with(vec![]);
            press(&mut app, KeyCode::Char('?'));
            assert_eq!(app.mode, Mode::Help);
            press(&mut app, KeyCode::Char('s'));
            assert_eq!(
                (&app.mode, app.sort),
                (&Mode::Help, SortKey::Cpu),
                "other keys do nothing"
            );
            press(&mut app, close);
            assert_eq!(app.mode, Mode::Normal, "{close:?} closes");
            assert!(!app.should_quit, "{close:?} does not quit");
        }
        let mut app = app_with(vec![]);
        press(&mut app, KeyCode::F(1));
        assert_eq!(app.mode, Mode::Help, "F1 opens");
    }

    #[test]
    fn digits_toggle_panels_and_zero_shows_all() {
        let mut app = app_with(vec![]);
        press(&mut app, KeyCode::Char('2'));
        assert!(!app.visible.contains(PanelId::History));
        press(&mut app, KeyCode::Char('8'));
        assert!(!app.visible.contains(PanelId::Processes));
        press(&mut app, KeyCode::Char('2'));
        assert!(app.visible.contains(PanelId::History));
        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.visible, PanelSet::ALL);
    }

    #[test]
    fn digit_for_a_panel_without_data_says_so() {
        let mut app = app_with(vec![]);
        for (key, name) in [('5', "storage"), ('6', "zfs"), ('7', "sensors")] {
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.status, Some(format!("{name}: not available here")));
            assert_eq!(app.visible, PanelSet::ALL, "{name} stays as it was");
        }

        app.handle(Event::Metrics(Snapshot {
            zfs: Some(ZfsArc::default()),
            storage: Storage {
                volumes: vec![Volume::default()],
                ..Storage::default()
            },
            sensors: Sensors {
                temps: vec![Temp::default()],
                battery: None,
            },
            ..Snapshot::default()
        }));
        for key in ['5', '6', '7'] {
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.status, None);
        }
        assert!(!app.visible.contains(PanelId::Storage));
        assert!(!app.visible.contains(PanelId::Zfs));
        assert!(!app.visible.contains(PanelId::Sensors));
    }

    #[test]
    fn ctrl_and_alt_keys_do_nothing_in_normal_mode() {
        let mut app = app_with(vec![proc(500, "sleep", 0.0, 1)]);
        draw(&mut app);
        press_with(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        press_with(&mut app, KeyCode::Char('q'), KeyModifiers::ALT);
        press_with(&mut app, KeyCode::Char('t'), KeyModifiers::ALT);
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.should_quit);
        assert!(!app.tree);
        press_with(&mut app, KeyCode::Char('G'), KeyModifiers::SHIFT);
        press_with(&mut app, KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(app.mode, Mode::Help, "shift is allowed");
    }

    #[test]
    fn letters_toggle_their_fields() {
        let mut app = app_with(vec![]);
        for key in ['t', 'r', 'c', 'n'] {
            press(&mut app, KeyCode::Char(key));
        }
        assert!(app.tree && app.reverse && app.show_cmd && app.net_detail);
        for key in ['t', 'r', 'c', 'n'] {
            press(&mut app, KeyCode::Char(key));
        }
        assert!(!app.tree && !app.reverse && !app.show_cmd && !app.net_detail);
    }

    #[test]
    fn reverse_and_tree_change_the_rows() {
        let mut app = app_with(vec![
            proc(1, "init", 0.0, 1),
            ProcInfo {
                parent: Some(1),
                ..proc(5, "child", 9.0, 1)
            },
        ]);
        assert_eq!(pids(&app), [5, 1]);
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(pids(&app), [1, 5]);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.visible_rows()[1].prefix, "└─");
    }

    #[test]
    fn show_cmd_also_filters_on_the_command_line() {
        let mut app = app_with(vec![ProcInfo {
            cmd: Arc::from("python3 -m http.server --port 8000"),
            ..proc(5, "python3", 0.0, 1)
        }]);
        app.filter = "--port".into();
        assert!(pids(&app).is_empty());
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(pids(&app), [5]);
    }

    #[test]
    fn apply_config_sets_each_field() {
        let mut app = app_with(vec![]);
        let config = Config {
            interval_ms: Some(10),
            sort: Some("mem".into()),
            reverse: Some(true),
            tree: Some(true),
            command: Some(true),
            panels: Some([PanelId::Cpu, PanelId::Processes].into_iter().collect()),
            fahrenheit: Some(true),
            net_detail: Some(true),
            warnings: Vec::new(),
        };
        app.apply_config(&config);
        assert_eq!(app.interval_ms(), MIN_INTERVAL_MS, "clamped up");
        assert_eq!(app.sort, SortKey::Memory);
        assert!(app.reverse && app.tree && app.show_cmd && app.fahrenheit && app.net_detail);
        assert_eq!(
            app.visible.iter().collect::<Vec<_>>(),
            [PanelId::Cpu, PanelId::Processes]
        );
        assert_eq!(app.status, None);

        app.apply_config(&Config {
            interval_ms: Some(1_000_000),
            ..Config::default()
        });
        assert_eq!(app.interval_ms(), MAX_INTERVAL_MS, "clamped down");
        assert_eq!(app.sort, SortKey::Memory, "unset fields stay");
    }

    #[test]
    fn apply_config_shows_the_first_warning() {
        let mut app = app_with(vec![]);
        app.apply_config(&Config {
            warnings: vec!["line 3: unknown key `colour`, ignored".into()],
            ..Config::default()
        });
        assert_eq!(
            app.status.as_deref(),
            Some("line 3: unknown key `colour`, ignored")
        );
        app.apply_config(&Config {
            warnings: vec!["a".into(), "b".into(), "c".into()],
            ..Config::default()
        });
        assert_eq!(app.status.as_deref(), Some("a (+2 more)"));
    }

    #[test]
    fn any_key_clears_the_status() {
        for key in [KeyCode::Char('j'), KeyCode::Char('?'), KeyCode::Null] {
            let mut app = app_with(vec![]);
            app.status = Some("old news".into());
            press(&mut app, key);
            assert_eq!(app.status, None, "{key:?}");
        }
        let mut app = app_with(vec![]);
        app.status = Some("old news".into());
        press_with(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(app.status, None, "an ignored key clears it too");
    }

    #[test]
    fn has_data_follows_the_snapshot() {
        let empty = Snapshot::default();
        for id in [PanelId::Storage, PanelId::Zfs, PanelId::Sensors] {
            assert!(!has_data(&empty, id), "{id:?}");
        }
        for id in [
            PanelId::Cpu,
            PanelId::History,
            PanelId::Memory,
            PanelId::Network,
            PanelId::Processes,
        ] {
            assert!(has_data(&empty, id), "{id:?}");
        }
    }
}
