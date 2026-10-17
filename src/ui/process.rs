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

/// The caller clamps the selection before the call.
pub fn draw_processes(
    frame: &mut Frame,
    area: Rect,
    table: &ProcTable<'_>,
    state: &mut TableState,
) {
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
                p.name.clone()
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
                start_time: 100,
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
            start_time: 100,
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
            start_time: 100,
            name: "x".repeat(200),
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
            start_time: 100,
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
    fn tiny_areas_do_not_panic() {
        let procs = sample_procs();
        let rows = rows(&procs);
        let target = KillTarget {
            pid: 1234,
            start_time: 1,
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
