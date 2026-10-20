//! Process panel drawing: the table and the kill prompt.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState};

use super::{DIM, FAINT, PINK, SELECTED, VIOLET, heat, panel};
use crate::format;
use crate::process::{KillTarget, ProcInfo, ProcRow, ProcState, SortKey};

/// Inner widths at which the table adds columns.
const ALL_COLUMNS_WIDTH: u16 = 96;
const MID_COLUMNS_WIDTH: u16 = 60;
/// The kill prompt size, before it is cut to the frame.
const PROMPT_WIDTH: u16 = 52;
const PROMPT_HEIGHT: u16 = 5;
const PROMPT_QUESTION: &str = "Send a signal to ";

pub struct ProcTable<'a> {
    pub rows: &'a [ProcRow<'a>],
    /// All processes before the filter.
    pub total: usize,
    pub sort: SortKey,
    pub reverse: bool,
    pub tree: bool,
    /// Show the command line in the last column, not the name.
    pub show_cmd: bool,
    pub filter: &'a str,
    /// False on Windows: hide the State and Threads columns.
    pub state_and_threads: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Pid,
    User,
    State,
    Threads,
    Cpu,
    Memory,
    Read,
    Write,
    Name,
}

impl Column {
    fn width(self) -> Constraint {
        match self {
            Column::Pid => Constraint::Length(7),
            Column::User => Constraint::Length(8),
            Column::State => Constraint::Length(1),
            Column::Threads => Constraint::Length(4),
            Column::Cpu => Constraint::Length(6),
            Column::Memory | Column::Read | Column::Write => Constraint::Length(10),
            Column::Name => Constraint::Fill(1),
        }
    }

    fn title(self, show_cmd: bool) -> &'static str {
        match self {
            Column::Pid => SortKey::Pid.title(),
            Column::User => "USER",
            Column::State => "S",
            Column::Threads => "THR",
            Column::Cpu => SortKey::Cpu.title(),
            Column::Memory => SortKey::Memory.title(),
            Column::Read => "READ/s",
            Column::Write => "WRITE/s",
            Column::Name if show_cmd => "COMMAND",
            Column::Name => SortKey::Name.title(),
        }
    }

    /// Numbers line up on the right.
    fn right_aligned(self) -> bool {
        !matches!(self, Column::User | Column::State | Column::Name)
    }

    /// True when the table is sorted by this column.
    fn sorts(self, key: SortKey) -> bool {
        matches!(
            (self, key),
            (Column::Pid, SortKey::Pid)
                | (Column::Name, SortKey::Name)
                | (Column::Cpu, SortKey::Cpu)
                | (Column::Memory, SortKey::Memory)
                | (Column::Read | Column::Write, SortKey::Io)
        )
    }
}

/// Pure. Columns for the inner table width (area width minus 2 borders and 1 highlight cell).
/// 96 or more: all. 60 to 95: no Read, Write. Under 60: Pid, Cpu, Memory, Name.
/// When `state_and_threads` is false, drop State and Threads.
pub fn columns_for_width(inner_width: u16, state_and_threads: bool) -> Vec<Column> {
    let all = [
        Column::Pid,
        Column::User,
        Column::State,
        Column::Threads,
        Column::Cpu,
        Column::Memory,
        Column::Read,
        Column::Write,
        Column::Name,
    ];
    all.into_iter()
        .filter(|&column| match column {
            Column::Pid | Column::Cpu | Column::Memory | Column::Name => true,
            Column::User => inner_width >= MID_COLUMNS_WIDTH,
            Column::State | Column::Threads => {
                state_and_threads && inner_width >= MID_COLUMNS_WIDTH
            }
            Column::Read | Column::Write => inner_width >= ALL_COLUMNS_WIDTH,
        })
        .collect()
}

/// Pure. The rows `start..end` that ratatui's `Table` shows, for `len` rows of 1 line each,
/// no row margins, and `height` lines for the rows (the area inside the block, less the header).
pub fn row_window(
    len: usize,
    offset: usize,
    selected: Option<usize>,
    height: u16,
) -> (usize, usize) {
    // `Table::visible_rows` in ratatui-widgets 0.3.2 (table.rs, about line 996), with each row
    // 1 line high, so its loops reduce to sums.
    if len == 0 {
        return (0, 0);
    }
    let height = usize::from(height);
    let last = len - 1;
    let mut start = offset.min(last);
    if let Some(selected) = selected {
        start = start.min(selected);
    }
    // Fill the lines from `start`.
    let mut end = start + height.min(len - start);
    // Scroll down until the selected row shows. Each step adds a row at the bottom and drops
    // one from the top, so the window ends on the selected row and keeps `height` rows.
    // With no lines at all, ratatui ends with `start == end == selected + 1`, and so does this.
    if let Some(selected) = selected.map(|s| s.min(last))
        && selected >= end
    {
        end = selected + 1;
        start = end - height;
    }
    // ratatui adds a partial row when lines are left over. Rows of 1 line leave none unless
    // the rows ran out, so this never fires; it stays to match ratatui.
    if end - start < height && end < len {
        end += 1;
    }
    (start, end)
}

/// The rows above the body inside the block: the header, with no margins.
const HEADER_HEIGHT: u16 = 1;

/// Builds `Row`s only for the rows that show, not for every process. `state` ends as
/// ratatui's `Table` would leave it after a draw of every row: the same selection clamp and
/// the same offset (see `row_window`).
pub fn draw_processes(
    frame: &mut Frame,
    area: Rect,
    table: &ProcTable<'_>,
    state: &mut TableState,
) {
    let block = panel(title(table));
    let inner = block.inner(area);
    let len = table.rows.len();
    // `Table::render` leaves the state alone when nothing fits inside the block.
    let window = if inner.is_empty() {
        0..0
    } else {
        if state.selected().is_some_and(|s| s >= len) {
            state.select(Some(len.saturating_sub(1)));
        }
        if len == 0 {
            // `select(None)` also sets the offset to 0, as in ratatui.
            state.select(None);
        }
        let (start, end) = row_window(
            len,
            state.offset(),
            state.selected(),
            inner.height.saturating_sub(HEADER_HEIGHT),
        );
        // `render_rows` returns before it sets the offset when there are no rows.
        if len > 0 {
            *state.offset_mut() = start;
        }
        start..end
    };
    // The window is drawn from its own top, so the selection moves with it. A selection
    // outside the window (only when no row fits) draws no highlight, as in ratatui.
    let mut window_state = TableState::default().with_selected(
        state
            .selected()
            .filter(|s| window.contains(s))
            .map(|s| s - window.start),
    );

    let columns = columns_for_width(area.width.saturating_sub(3), table.state_and_threads);
    let descending = table.sort.descending() != table.reverse;

    let header = Row::new(columns.iter().map(|&column| {
        let (text, style) = if column.sorts(table.sort) {
            let arrow = if descending { "▼" } else { "▲" };
            (
                format!("{} {arrow}", column.title(table.show_cmd)),
                Style::new().fg(PINK).add_modifier(Modifier::BOLD),
            )
        } else {
            (
                column.title(table.show_cmd).to_owned(),
                Style::new().fg(DIM).add_modifier(Modifier::BOLD),
            )
        };
        Cell::from(align(Line::from(text), column)).style(style)
    }));

    let rows = table.rows[window].iter().map(|row| {
        Row::new(
            columns
                .iter()
                .map(|&column| Cell::from(align(cell(row, column, table.show_cmd), column))),
        )
    });

    let widget = Table::new(rows, columns.iter().map(|column| column.width()))
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(widget, area, &mut window_state);
}

fn title(table: &ProcTable<'_>) -> String {
    let mut title = format!(
        " processes {}/{} · sort {}",
        table.rows.len(),
        table.total,
        table.sort.name()
    );
    if table.reverse {
        title.push_str(" rev");
    }
    if table.tree {
        title.push_str(" · tree");
    }
    if !table.filter.is_empty() {
        title.push_str(&format!(" · filter \"{}\"", table.filter));
    }
    title.push(' ');
    title
}

fn align(line: Line<'static>, column: Column) -> Line<'static> {
    if column.right_aligned() {
        line.right_aligned()
    } else {
        line
    }
}

/// `-` for a value that this OS does not give.
fn dash() -> Line<'static> {
    Line::styled("-", FAINT)
}

fn cell(row: &ProcRow<'_>, column: Column, show_cmd: bool) -> Line<'static> {
    let p: &ProcInfo = row.proc;
    match column {
        Column::Pid => Line::styled(p.pid.to_string(), DIM),
        Column::User => p
            .user
            .as_deref()
            .map_or_else(dash, |user| Line::raw(user.to_owned())),
        Column::State => {
            let style = match p.state {
                ProcState::Run => Style::new().fg(VIOLET),
                ProcState::DiskSleep | ProcState::Zombie => Style::new().fg(PINK),
                _ => Style::new().fg(DIM),
            };
            Line::styled(p.state.code().to_string(), style)
        }
        Column::Threads => p.threads.map_or_else(dash, |n| Line::raw(n.to_string())),
        Column::Cpu => Line::styled(format!("{:.1}", p.cpu), heat(f64::from(p.cpu))),
        Column::Memory => Line::raw(format::bytes(p.memory)),
        Column::Read => io_line(p.read_rate),
        Column::Write => io_line(p.write_rate),
        Column::Name => {
            let text = if !show_cmd {
                p.name.to_string()
            } else if p.cmd.is_empty() {
                // Kernel threads and processes whose command line is not read yet.
                format!("[{}]", p.name)
            } else {
                p.cmd.to_string()
            };
            Line::from(vec![
                Span::styled(row.prefix.clone(), FAINT),
                Span::raw(text),
            ])
        }
    }
}

/// Most processes do no disk I/O, so a quiet `0` keeps the eye on the busy ones.
fn io_line(rate: u64) -> Line<'static> {
    if rate == 0 {
        Line::styled("0", FAINT)
    } else {
        Line::raw(format::bytes(rate))
    }
}

/// Draws a centred dialog over `area` (the full frame). Clears the cells under it first.
pub fn draw_kill_prompt(frame: &mut Frame, area: Rect, target: &KillTarget) {
    let width = area.width.min(PROMPT_WIDTH);
    let height = area.height.min(PROMPT_HEIGHT);
    let dialog = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, dialog);

    let pid = target.pid.to_string();
    // Room for the name: the inner width less the question, the pid, a space and "?".
    let room = usize::from(width.saturating_sub(2))
        .saturating_sub(PROMPT_QUESTION.chars().count() + pid.len() + 2);
    let key = |k: &'static str| Span::styled(k, Style::new().fg(PINK).add_modifier(Modifier::BOLD));
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let text = vec![
        Line::from(vec![
            Span::raw(PROMPT_QUESTION),
            Span::styled(pid, bold),
            Span::raw(" "),
            Span::styled(cut(&target.name, room), bold),
            Span::raw("?"),
        ]),
        Line::default(),
        Line::from(vec![
            key("y"),
            Span::raw(" TERM"),
            Span::styled(" · ", FAINT),
            key("f"),
            Span::raw(" KILL (force)"),
            Span::styled(" · ", FAINT),
            key("n"),
            Span::raw(" cancel"),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(text).block(panel(" kill ")).centered(),
        dialog,
    );
}

/// Cuts `text` to `max` chars, with `…` as the last char when it cuts.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    if max > 0 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::render_to_string;
    use super::*;
    use crate::process::ProcInfo;

    const ALL: [Column; 9] = [
        Column::Pid,
        Column::User,
        Column::State,
        Column::Threads,
        Column::Cpu,
        Column::Memory,
        Column::Read,
        Column::Write,
        Column::Name,
    ];

    #[test]
    fn wide_tables_show_every_column() {
        assert_eq!(columns_for_width(120, true), ALL);
        assert_eq!(columns_for_width(96, true), ALL);
    }

    #[test]
    fn medium_tables_drop_read_and_write() {
        let expected = [
            Column::Pid,
            Column::User,
            Column::State,
            Column::Threads,
            Column::Cpu,
            Column::Memory,
            Column::Name,
        ];
        assert_eq!(columns_for_width(80, true), expected);
        assert_eq!(columns_for_width(95, true), expected);
        assert_eq!(columns_for_width(60, true), expected);
    }

    #[test]
    fn narrow_tables_keep_four_columns() {
        let expected = [Column::Pid, Column::Cpu, Column::Memory, Column::Name];
        assert_eq!(columns_for_width(50, true), expected);
        assert_eq!(columns_for_width(59, true), expected);
        assert_eq!(columns_for_width(0, true), expected);
        assert_eq!(columns_for_width(50, false), expected);
    }

    #[test]
    fn no_state_and_threads_drops_both_columns() {
        let columns = columns_for_width(120, false);
        assert!(!columns.contains(&Column::State));
        assert!(!columns.contains(&Column::Threads));
        assert_eq!(columns.len(), 7);
        assert_eq!(columns_for_width(80, false).len(), 5);
    }

    fn sample_procs() -> Vec<ProcInfo> {
        vec![
            ProcInfo {
                pid: 4242,
                parent: Some(1),
                ident: 100,
                name: "cargo".into(),
                cmd: "cargo build --release".into(),
                user: Some("alice".into()),
                state: crate::process::ProcState::Run,
                threads: Some(12),
                cpu: 87.5,
                memory: 300 << 20,
                read_rate: 2048,
                write_rate: 0,
            },
            ProcInfo {
                pid: 7,
                name: "kworker".into(),
                ..ProcInfo::default()
            },
        ]
    }

    fn table<'a>(rows: &'a [ProcRow<'a>]) -> ProcTable<'a> {
        ProcTable {
            rows,
            total: 2,
            sort: SortKey::Cpu,
            reverse: false,
            tree: false,
            show_cmd: false,
            filter: "",
            state_and_threads: true,
        }
    }

    fn rows(procs: &[ProcInfo]) -> Vec<ProcRow<'_>> {
        procs
            .iter()
            .map(|proc| ProcRow {
                proc,
                prefix: String::new(),
            })
            .collect()
    }

    fn render(width: u16, height: u16, table: &ProcTable<'_>) -> String {
        let mut state = TableState::default().with_selected(0);
        render_to_string(width, height, |frame| {
            draw_processes(frame, frame.area(), table, &mut state);
        })
    }

    #[test]
    fn table_shows_header_title_and_rows() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let screen = render(110, 10, &table(&rows));
        for text in [
            "processes 2/2 · sort cpu",
            "PID",
            "USER",
            "THR",
            "CPU% ▼",
            "MEM",
            "READ/s",
            "WRITE/s",
            "NAME",
            "4242",
            "alice",
            "12",
            "87.5",
            "300.0 MiB",
            "2.0 KiB",
            "cargo",
            "kworker",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        assert!(!screen.contains("COMMAND"), "{screen}");
        assert!(!screen.contains("cargo build"), "{screen}");
    }

    #[test]
    fn four_digit_byte_values_keep_every_digit() {
        let mut procs = sample_procs();
        procs[0].memory = 1010 << 20;
        procs[0].read_rate = 1000 << 20;
        procs[0].write_rate = 1023 << 30;
        let rows = rows(&procs);
        // 96 inner cells is the narrowest table that shows Read and Write.
        for width in [120, 99] {
            let screen = render(width, 10, &table(&rows));
            for text in ["1010.0 MiB", "1000.0 MiB", "1023.0 GiB"] {
                assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
            }
        }
    }

    #[test]
    fn title_shows_reverse_tree_and_filter() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let t = ProcTable {
            sort: SortKey::Memory,
            reverse: true,
            tree: true,
            filter: "car",
            ..table(&rows)
        };
        let screen = render(110, 10, &t);
        assert!(
            screen.contains("processes 2/2 · sort mem rev · tree · filter \"car\""),
            "{screen}"
        );
        assert!(
            screen.contains("MEM ▲"),
            "reverse flips the arrow:\n{screen}"
        );
    }

    #[test]
    fn io_sort_marks_read_and_write() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let t = ProcTable {
            sort: SortKey::Io,
            ..table(&rows)
        };
        let screen = render(110, 10, &t);
        assert!(screen.contains("READ/s ▼"), "{screen}");
        assert!(screen.contains("WRITE/s ▼"), "{screen}");
        let narrow = render(70, 10, &t);
        assert!(narrow.contains("sort io"), "{narrow}");
        assert!(!narrow.contains("READ/s"), "{narrow}");
    }

    #[test]
    fn show_cmd_shows_the_command_line() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let t = ProcTable {
            show_cmd: true,
            ..table(&rows)
        };
        let screen = render(110, 10, &t);
        assert!(screen.contains("COMMAND"), "{screen}");
        assert!(screen.contains("cargo build --release"), "{screen}");
        assert!(
            screen.contains("[kworker]"),
            "empty cmd shows [name]:\n{screen}"
        );
    }

    #[test]
    fn tree_prefix_shows_before_the_name() {
        let procs = sample_procs();
        let rows = vec![
            ProcRow {
                proc: &procs[1],
                prefix: String::new(),
            },
            ProcRow {
                proc: &procs[0],
                prefix: "│ └─".into(),
            },
        ];
        let t = ProcTable {
            tree: true,
            ..table(&rows)
        };
        let screen = render(110, 10, &t);
        assert!(screen.contains("│ └─cargo"), "{screen}");
        let with_cmd = render(
            110,
            10,
            &ProcTable {
                show_cmd: true,
                ..t
            },
        );
        assert!(with_cmd.contains("│ └─cargo build"), "{with_cmd}");
    }

    #[test]
    fn windows_table_hides_state_and_threads() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let t = ProcTable {
            state_and_threads: false,
            ..table(&rows)
        };
        let screen = render(110, 10, &t);
        assert!(!screen.contains("THR"), "{screen}");
        assert!(screen.contains("USER"), "{screen}");
    }

    #[test]
    fn missing_values_show_a_dash() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let screen = render(110, 10, &table(&rows));
        let kworker = screen
            .lines()
            .find(|line| line.contains("kworker"))
            .unwrap();
        assert!(kworker.contains(" - "), "{kworker}");
        assert!(kworker.contains('?'), "unknown state:\n{kworker}");
    }

    #[test]
    fn kill_prompt_shows_target_and_keys() {
        let target = KillTarget {
            pid: 4242,
            ident: 100,
            name: "cargo".into(),
        };
        let screen = render_to_string(80, 24, |frame| {
            draw_kill_prompt(frame, frame.area(), &target);
        });
        for text in [
            "kill",
            "Send a signal to 4242 cargo?",
            "y TERM",
            "f KILL",
            "n cancel",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
    }

    #[test]
    fn kill_prompt_cuts_a_long_name() {
        let target = KillTarget {
            pid: 4242,
            ident: 100,
            name: "x".repeat(200).into(),
        };
        let screen = render_to_string(80, 24, |frame| {
            draw_kill_prompt(frame, frame.area(), &target);
        });
        assert!(screen.contains("4242 xxx"), "{screen}");
        assert!(screen.contains("…?"), "{screen}");
    }

    #[test]
    fn kill_prompt_clears_the_cells_under_it() {
        let target = KillTarget {
            pid: 4242,
            ident: 100,
            name: "cargo".into(),
        };
        let screen = render_to_string(80, 24, |frame| {
            let filler = "#".repeat(80 * 24);
            frame.render_widget(
                ratatui::widgets::Paragraph::new(filler)
                    .wrap(ratatui::widgets::Wrap { trim: false }),
                frame.area(),
            );
            draw_kill_prompt(frame, frame.area(), &target);
        });
        let line = screen
            .lines()
            .find(|line| line.contains("Send a signal"))
            .unwrap();
        assert!(line.contains("  Send"), "{line}");
    }

    #[test]
    fn row_window_starts_at_the_top() {
        assert_eq!(row_window(100, 0, Some(0), 10), (0, 10));
        assert_eq!(
            row_window(5, 0, Some(0), 10),
            (0, 5),
            "fewer rows than lines"
        );
        assert_eq!(row_window(0, 7, None, 10), (0, 0), "no rows");
    }

    #[test]
    fn row_window_keeps_the_offset_while_the_selection_shows() {
        assert_eq!(row_window(100, 3, Some(7), 10), (3, 13));
        assert_eq!(row_window(100, 3, None, 10), (3, 13));
        assert_eq!(
            row_window(100, 95, Some(99), 10),
            (95, 100),
            "ratatui does not pull the offset back to fill the view"
        );
    }

    #[test]
    fn row_window_scrolls_to_the_selection() {
        assert_eq!(row_window(100, 0, Some(25), 10), (16, 26), "down");
        assert_eq!(row_window(100, 50, Some(20), 10), (20, 30), "up");
        assert_eq!(
            row_window(10, 0, Some(50), 4),
            (6, 10),
            "past the end: the last row"
        );
    }

    #[test]
    fn row_window_handles_stale_offsets_and_no_room() {
        assert_eq!(
            row_window(100, 500, None, 10),
            (99, 100),
            "offset past the end"
        );
        assert_eq!(
            row_window(100, 0, Some(5), 0),
            (6, 6),
            "no lines: as ratatui"
        );
        assert_eq!(row_window(100, 4, None, 0), (4, 4));
    }

    // The equivalence tests: the windowed table against the old one, which builds a row for
    // every process and lets ratatui pick the window.

    /// The old `draw_processes`, kept as the reference.
    fn draw_full(frame: &mut Frame, area: Rect, table: &ProcTable<'_>, state: &mut TableState) {
        let columns = columns_for_width(area.width.saturating_sub(3), table.state_and_threads);
        let descending = table.sort.descending() != table.reverse;
        let header = Row::new(columns.iter().map(|&column| {
            let (text, style) = if column.sorts(table.sort) {
                let arrow = if descending { "▼" } else { "▲" };
                (
                    format!("{} {arrow}", column.title(table.show_cmd)),
                    Style::new().fg(PINK).add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    column.title(table.show_cmd).to_owned(),
                    Style::new().fg(DIM).add_modifier(Modifier::BOLD),
                )
            };
            Cell::from(align(Line::from(text), column)).style(style)
        }));
        let rows = table.rows.iter().map(|row| {
            Row::new(
                columns
                    .iter()
                    .map(|&column| Cell::from(align(cell(row, column, table.show_cmd), column))),
            )
        });
        let widget = Table::new(rows, columns.iter().map(|column| column.width()))
            .header(header)
            .block(panel(title(table)))
            .column_spacing(1)
            .row_highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD))
            .highlight_symbol("▌")
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(widget, area, state);
    }

    fn draw_to_buffer(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut Frame),
    ) -> ratatui::buffer::Buffer {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(draw).unwrap();
        terminal.backend().buffer().clone()
    }

    /// `n` processes, `worker-0` busiest, with every column filled.
    fn many_procs(n: usize) -> Vec<ProcInfo> {
        (0..n)
            .map(|i| ProcInfo {
                pid: 1_000 + i as u32,
                parent: Some(1),
                ident: i as u64,
                name: format!("worker-{i}").into(),
                cmd: format!("/bin/worker-{i} --id {i}").into(),
                user: Some("alice".into()),
                state: crate::process::ProcState::Run,
                threads: Some(2),
                cpu: (n - i) as f32,
                memory: (i as u64) << 20,
                read_rate: (i as u64 % 3) * 1024,
                write_rate: 0,
            })
            .collect()
    }

    #[test]
    fn windowed_table_matches_the_full_table() {
        let mut cases = 0;
        for len in [0, 1, 3, 9, 40, 600] {
            let procs = many_procs(len);
            let rows = rows(&procs);
            let t = ProcTable {
                total: len,
                ..table(&rows)
            };
            for (width, height) in [(110, 1), (110, 2), (110, 3), (110, 4), (110, 5), (110, 12)]
                .into_iter()
                .chain([(50, 8), (110, 40), (2, 6)])
            {
                let selections = [None, Some(0), Some(len / 2), Some(len.saturating_sub(1))]
                    .into_iter()
                    .chain([Some(len), Some(len + 50)]);
                for selected in selections {
                    for offset in [0, 2, len / 2, len.saturating_sub(1), len + 10] {
                        let start = TableState::default()
                            .with_offset(offset)
                            .with_selected(selected);
                        let mut want_state = start;
                        let want = draw_to_buffer(width, height, |frame| {
                            draw_full(frame, frame.area(), &t, &mut want_state);
                        });
                        let mut got_state = start;
                        let got = draw_to_buffer(width, height, |frame| {
                            draw_processes(frame, frame.area(), &t, &mut got_state);
                        });
                        let case = format!(
                            "{len} rows, {width}x{height}, selected {selected:?}, offset {offset}"
                        );
                        assert_eq!(got, want, "{case}");
                        assert_eq!(got_state, want_state, "{case}");
                        cases += 1;
                    }
                }
            }
        }
        assert!(cases > 1_000, "{cases}");
    }

    /// The old `ui::draw_processes`, drawn with the full table.
    fn draw_app_full(
        frame: &mut Frame,
        area: Rect,
        app: &mut crate::app::App,
    ) -> Option<KillTarget> {
        let rows = app.visible_rows();
        let mut state = app.table;
        crate::app::clamp_selection(&mut state, rows.len());
        let highlighted = state
            .selected()
            .and_then(|i| rows.get(i))
            .map(|row| KillTarget {
                pid: row.proc.pid,
                ident: row.proc.ident,
                name: row.proc.name.clone(),
            });
        let table = ProcTable {
            rows: &rows,
            total: app.snapshot.as_ref().map_or(0, |s| s.processes.len()),
            sort: app.sort,
            reverse: app.reverse,
            tree: app.tree,
            show_cmd: app.show_cmd,
            filter: &app.filter,
            state_and_threads: !cfg!(windows),
        };
        draw_full(frame, area, &table, &mut state);
        drop(rows);
        app.table = state;
        highlighted
    }

    enum Step {
        Keys(&'static str),
        Key(ratatui::crossterm::event::KeyCode),
        Resize(u16),
        /// A new snapshot with this many processes, as when processes exit.
        Processes(usize),
    }

    #[test]
    fn key_sequences_leave_the_same_state_as_the_full_table() {
        use crate::app::{App, Event};
        use crate::metrics::{HostInfo, Snapshot};
        use Step::{Key, Keys, Processes, Resize};
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let snapshot = |n| {
            Event::Metrics(Snapshot {
                processes: many_procs(n),
                ..Snapshot::default()
            })
        };
        let script = [
            Keys("jjjjjjjjjjjj"),
            Key(KeyCode::PageDown),
            Keys("k"),
            Key(KeyCode::PageDown),
            Key(KeyCode::PageDown),
            Resize(6),
            Key(KeyCode::PageUp),
            Keys("G"),
            Resize(30),
            Keys("kkk"),
            Resize(4),
            Keys("g"),
            Key(KeyCode::PageDown),
            Resize(3),
            Keys("jj"),
            Resize(14),
            // The filter: `/worker-1` keeps 11 of 60 rows.
            Keys("/worker-1"),
            Key(KeyCode::Enter),
            Keys("G"),
            Keys("k"),
            Key(KeyCode::Esc),
            Keys("jjj"),
            Key(KeyCode::PageDown),
            Processes(5),
            Keys("kG"),
            Processes(0),
            Keys("jk"),
            Processes(60),
            Keys("Gkkkkkkkkkkkkkkkkk"),
            Key(KeyCode::PageUp),
            Keys("/zzz"),
            Key(KeyCode::Enter),
            Keys("jG"),
            Key(KeyCode::Esc),
            Keys("j"),
        ];

        for (width, start_height) in [(110, 12), (110, 5), (110, 3), (50, 20), (110, 40)] {
            let mut windowed = App::new(HostInfo::default(), Arc::new(AtomicU64::new(1_000)));
            let mut full = App::new(HostInfo::default(), Arc::new(AtomicU64::new(1_000)));
            windowed.handle(snapshot(60));
            full.handle(snapshot(60));
            let mut height = start_height;
            for (i, step) in script.iter().enumerate() {
                let codes: Vec<KeyCode> = match step {
                    Keys(keys) => keys.chars().map(KeyCode::Char).collect(),
                    Key(code) => vec![*code],
                    Resize(h) => {
                        height = *h;
                        Vec::new()
                    }
                    Processes(n) => {
                        windowed.handle(snapshot(*n));
                        full.handle(snapshot(*n));
                        Vec::new()
                    }
                };
                for code in codes {
                    windowed.handle(Event::Key(KeyEvent::from(code)));
                    full.handle(Event::Key(KeyEvent::from(code)));
                }
                let mut got_target = None;
                let got = draw_to_buffer(width, height, |frame| {
                    got_target = super::super::draw_processes(frame, frame.area(), &mut windowed);
                });
                let mut want_target = None;
                let want = draw_to_buffer(width, height, |frame| {
                    want_target = draw_app_full(frame, frame.area(), &mut full);
                });
                let case = format!("{width}x{start_height}, step {i}");
                assert_eq!(got, want, "{case}");
                assert_eq!(windowed.table, full.table, "{case}");
                assert_eq!(got_target, want_target, "{case}");
            }
        }
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let target = KillTarget {
            pid: 1234,
            ident: 1,
            name: "zsh".into(),
        };
        for (w, h) in [(1, 1), (10, 3), (3, 1), (200, 2)] {
            render(w, h, &table(&rows));
            render_to_string(w, h, |frame| {
                draw_kill_prompt(frame, frame.area(), &target);
            });
        }
        let mut state = TableState::default().with_selected(0);
        render_to_string(20, 10, |frame| {
            draw_processes(frame, Rect::new(5, 5, 0, 0), &table(&rows), &mut state);
            draw_kill_prompt(frame, Rect::new(5, 5, 0, 0), &target);
        });
    }
}
