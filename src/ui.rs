//! Rendering. Each panel is a function of the current state, and ratatui
//! diffs the frames, so only the changed cells reach the terminal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::{self, Marker};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, LineGauge, Paragraph, Sparkline};

use crate::app::{self, App, HISTORY_LEN, History, Mode};
use crate::format;
use crate::layout::{self, PROC_MIN_H, PanelReq};
use crate::metrics::Snapshot;
use crate::panel::PanelId;
use crate::process::KillTarget;

mod help;
mod net;
mod process;
mod sensors;
mod storage;
mod zfs;

const PURPLE: Color = Color::Rgb(124, 58, 237);
const VIOLET: Color = Color::Rgb(167, 139, 250);
const PINK: Color = Color::Rgb(244, 114, 182);
const BLUE: Color = Color::Rgb(96, 165, 250);
const DIM: Color = Color::Rgb(110, 106, 134);
const FAINT: Color = Color::Rgb(58, 55, 74);
const SELECTED: Color = Color::Rgb(59, 31, 107);
const INK: Color = Color::Rgb(16, 14, 22);

/// The most core rows to stack before the CPU panel grows another column.
const MAX_CORE_ROWS: usize = 12;
/// No panel asks layout for more lines than this; the rest goes to the process table.
const MAX_WANT_H: u16 = 14;

/// Blue when calm, violet when busy, pink when hot.
fn heat(pct: f64) -> Color {
    match pct {
        p if p < 40.0 => BLUE,
        p if p < 75.0 => VIOLET,
        _ => PINK,
    }
}

fn panel<'a>(title: impl Into<String>) -> Block<'a> {
    Block::bordered()
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(DIM)
        .title(Span::styled(
            title.into(),
            Style::new().fg(VIOLET).add_modifier(Modifier::BOLD),
        ))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_header(frame, header, app);

    let hidden = draw_body(frame, body, app);
    draw_footer(frame, footer, app, hidden);

    // Overlays go last, over the panels.
    match &app.mode {
        Mode::Help => help::draw_help(frame, frame.area()),
        Mode::ConfirmKill(target) => {
            process::draw_kill_prompt(frame, frame.area(), target);
            app.kill_prompt_drawn = true;
        }
        Mode::Normal | Mode::Filter => {}
    }
}

/// Draws the panels that fit. Returns how many panels layout collapsed to make room.
fn draw_body(frame: &mut Frame, body: Rect, app: &mut App) -> usize {
    let Some(snapshot) = app.snapshot.as_ref() else {
        let waiting = Paragraph::new("sampling…").style(DIM).centered();
        frame.render_widget(waiting, body.centered_vertically(Constraint::Length(1)));
        app.highlighted = None;
        return 0;
    };

    let plan = layout::plan(body, &requests(app, snapshot));
    app.publish_drawn(plan.areas.iter().map(|&(id, _)| id).collect());
    for &(id, area) in &plan.areas {
        match id {
            PanelId::Cpu => draw_cores(frame, area, snapshot),
            PanelId::History => draw_history(frame, area, app),
            PanelId::Memory => draw_memory(frame, area, app, snapshot),
            PanelId::Network if app.net_detail => {
                net::draw_interfaces(frame, area, &snapshot.interfaces);
            }
            PanelId::Network => draw_network(frame, area, app, snapshot),
            PanelId::Storage => storage::draw_storage(frame, area, &snapshot.storage),
            PanelId::Zfs => {
                if let Some(arc) = &snapshot.zfs {
                    zfs::draw_zfs(frame, area, arc);
                }
            }
            PanelId::Sensors => {
                sensors::draw_sensors(frame, area, &snapshot.sensors, app.fahrenheit);
            }
            // The table needs `app` mutably, so it is drawn after this loop.
            PanelId::Processes => {}
        }
    }

    app.highlighted = plan
        .area(PanelId::Processes)
        .and_then(|area| draw_processes(frame, area, app));
    plan.collapsed.len()
}

/// One request for each panel that the user shows and that has data on this host (§7.5).
fn requests(app: &App, snapshot: &Snapshot) -> Vec<PanelReq> {
    let cores = snapshot.cpus.len();
    let core_rows = cores.div_ceil(core_columns(cores));
    let cpu_want = u16::try_from(core_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    // A panel with no data on this host takes no space.
    let mut shown = app.visible;
    for id in PanelId::ALL {
        if !app::has_data(snapshot, id) {
            shown.remove(id);
        }
    }
    shown
        .iter()
        .map(|id| {
            let (min_h, want_h, min_w) = match id {
                PanelId::Cpu => (4, cpu_want, 20),
                PanelId::History => (6, 8, 20),
                PanelId::Memory => (5, 5, 30),
                PanelId::Network if app.net_detail => {
                    (net::MIN_HEIGHT, net::want_height(&snapshot.interfaces), 24)
                }
                PanelId::Network => (4, 5, 24),
                PanelId::Storage => (
                    storage::MIN_HEIGHT,
                    storage::want_height(&snapshot.storage),
                    storage::MIN_WIDTH,
                ),
                PanelId::Zfs => (
                    zfs::MIN_HEIGHT,
                    snapshot.zfs.as_ref().map_or(0, zfs::want_height),
                    zfs::MIN_WIDTH,
                ),
                PanelId::Sensors => (
                    sensors::MIN_HEIGHT,
                    sensors::want_height(&snapshot.sensors),
                    sensors::MIN_WIDTH,
                ),
                PanelId::Processes => (PROC_MIN_H, PROC_MIN_H, 0),
            };
            PanelReq {
                id,
                min_h,
                want_h: want_h.max(min_h).min(MAX_WANT_H),
                min_w,
            }
        })
        .collect()
}

fn core_columns(cores: usize) -> usize {
    let preferred = if cores > 6 { 2 } else { 1 };
    preferred.max(cores.div_ceil(MAX_CORE_ROWS))
}

/// Columns for the core list: today's choice, more when the area is short,
/// at most one column for each 14 inner cells of width.
fn core_grid(cores: usize, inner: Rect) -> usize {
    let max_cols = ((usize::from(inner.width) + 2) / 14).max(1);
    let by_height = cores.div_ceil(usize::from(inner.height).max(1));
    core_columns(cores).max(by_height).min(max_cols)
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let sep = || Span::styled(" · ", Style::new().fg(FAINT));
    let host = &app.host;
    let mut spans = vec![
        Span::styled(
            " rtop ",
            Style::new()
                .fg(Color::White)
                .bg(PURPLE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            host.hostname.clone(),
            Style::new().fg(VIOLET).add_modifier(Modifier::BOLD),
        ),
        sep(),
        Span::raw(host.os.clone()),
        sep(),
        // Some Linux arm64 kernels report no CPU brand.
        Span::raw(
            format!("{} {}", host.cpu_brand, host.arch)
                .trim()
                .to_owned(),
        ),
    ];
    if let Some(s) = &app.snapshot {
        spans.extend([
            sep(),
            Span::styled("up ", DIM),
            Span::raw(format::duration(s.uptime)),
            sep(),
            Span::styled("load ", DIM),
            Span::raw(format!(
                "{:.2} {:.2} {:.2}",
                s.load[0], s.load[1], s.load[2]
            )),
        ]);
    }
    spans.extend([
        sep(),
        Span::styled("every ", DIM),
        Span::raw(format!("{:.2}s", app.interval_ms() as f64 / 1000.0)),
    ]);
    if app.paused {
        spans.extend([
            Span::raw("  "),
            Span::styled(
                " PAUSED ",
                Style::new().fg(INK).bg(PINK).add_modifier(Modifier::BOLD),
            ),
        ]);
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_cores(frame: &mut Frame, area: Rect, snapshot: &Snapshot) {
    let block = panel(format!(" cpu {:.0}% ", snapshot.cpu_total));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = core_grid(snapshot.cpus.len(), inner);
    // When even the widest grid cannot hold every core, show the first ones in order.
    let rows = snapshot
        .cpus
        .len()
        .div_ceil(cols)
        .min(usize::from(inner.height).max(1));
    let columns = Layout::horizontal(vec![Constraint::Fill(1); cols])
        .spacing(2)
        .split(inner);
    for (col, rect) in columns.iter().enumerate() {
        let lines: Vec<Line> = snapshot
            .cpus
            .iter()
            .enumerate()
            .skip(col * rows)
            .take(rows)
            .map(|(i, &pct)| core_line(i, f64::from(pct), rect.width as usize))
            .collect();
        frame.render_widget(Paragraph::new(lines), *rect);
    }
}

fn core_line(index: usize, pct: f64, width: usize) -> Line<'static> {
    let bar_width = width.saturating_sub("00 ".len() + " 100%".len());
    let bar = format::meter(pct / 100.0, bar_width);
    let rest = bar_width.saturating_sub(bar.chars().count());
    Line::from(vec![
        Span::styled(format!("{index:>2} "), DIM),
        Span::styled(bar, heat(pct)),
        Span::styled("·".repeat(rest), FAINT),
        Span::raw(format!(" {pct:>3.0}%")),
    ])
}

/// Places the newest sample at the right edge of the chart.
fn chart_points(history: &History<f64>) -> Vec<(f64, f64)> {
    let offset = HISTORY_LEN - history.len();
    history
        .iter()
        .enumerate()
        .map(|(i, v)| ((offset + i) as f64, v))
        .collect()
}

/// The dataset borrows `data`, so the compiler makes sure the points outlive the chart.
fn series<'a>(name: &'a str, color: Color, data: &'a [(f64, f64)]) -> Dataset<'a> {
    Dataset::default()
        .name(name)
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(color)
        .data(data)
}

fn draw_history(frame: &mut Frame, area: Rect, app: &App) {
    let cpu = chart_points(&app.cpu_history);
    let mem = chart_points(&app.mem_history);
    let chart = Chart::new(vec![series("cpu", VIOLET, &cpu), series("mem", PINK, &mem)])
        .block(panel(format!(" history · last {HISTORY_LEN} samples ")))
        .x_axis(
            Axis::default()
                .bounds([0.0, (HISTORY_LEN - 1) as f64])
                .style(FAINT),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(["0", "50", "100"])
                .style(DIM),
        );
    frame.render_widget(chart, area);
}

fn gauge(label: &str, used: u64, total: u64) -> LineGauge<'static> {
    let pct = format::percent(used, total);
    LineGauge::default()
        .ratio((pct / 100.0).clamp(0.0, 1.0))
        .label(Line::from(vec![
            Span::styled(format!("{label:<5}"), DIM),
            Span::raw(format!(
                "{:>9} / {:<9}",
                format::bytes(used),
                format::bytes(total)
            )),
        ]))
        .filled_style(heat(pct))
        .unfilled_style(FAINT)
        .filled_symbol(symbols::line::THICK_HORIZONTAL)
        .unfilled_symbol(symbols::line::THICK_HORIZONTAL)
}

fn draw_memory(frame: &mut Frame, area: Rect, app: &App, snapshot: &Snapshot) {
    let block = panel(format!(
        " memory {:.0}% ",
        format::percent(snapshot.mem_used, snapshot.mem_total)
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [ram, swap, info] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);
    frame.render_widget(gauge("ram", snapshot.mem_used, snapshot.mem_total), ram);
    frame.render_widget(gauge("swap", snapshot.swap_used, snapshot.swap_total), swap);

    let physical = app
        .host
        .physical_cores
        .map_or_else(String::new, |n| format!(" ({n} physical)"));
    let arc = snapshot.zfs.as_ref().map_or_else(String::new, |arc| {
        format!(" · arc {}", format::bytes(arc.size))
    });
    let text = format!(
        "{} processes · {} cores{physical}{arc}",
        snapshot.processes.len(),
        snapshot.cpus.len()
    );
    frame.render_widget(Paragraph::new(text).style(DIM), info);
}

/// The newest samples that fit in `width` columns, oldest first.
fn tail(history: &History<u64>, width: u16) -> Vec<u64> {
    history
        .iter()
        .skip(history.len().saturating_sub(width as usize))
        .collect()
}

fn draw_network(frame: &mut Frame, area: Rect, app: &App, snapshot: &Snapshot) {
    let block = panel(" network ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [labels, sparks] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    let halves = Layout::horizontal([Constraint::Fill(1); 2]).spacing(2);
    let [rx_label, tx_label] = halves.areas(labels);
    let [rx_spark, tx_spark] = halves.areas(sparks);

    for (label_area, spark_area, arrow, rate, history, color) in [
        (
            rx_label,
            rx_spark,
            "↓ rx ",
            snapshot.rx_rate,
            &app.rx_history,
            BLUE,
        ),
        (
            tx_label,
            tx_spark,
            "↑ tx ",
            snapshot.tx_rate,
            &app.tx_history,
            PINK,
        ),
    ] {
        let label = Line::from(vec![
            Span::styled(arrow, color),
            Span::raw(format::rate(rate)),
        ]);
        frame.render_widget(label, label_area);
        let data = tail(history, spark_area.width);
        frame.render_widget(Sparkline::default().data(&data).style(color), spark_area);
    }
}

/// Draws the process table. Returns the row that it highlighted, which `x` kills.
fn draw_processes(frame: &mut Frame, area: Rect, app: &mut App) -> Option<KillTarget> {
    let rows = app.visible_rows();
    let mut state = app.table;
    app::clamp_selection(&mut state, rows.len());
    let highlighted = state
        .selected()
        .and_then(|i| rows.get(i))
        .map(|row| KillTarget {
            pid: row.proc.pid,
            ident: row.proc.ident,
            name: row.proc.name.clone(),
        });
    let table = process::ProcTable {
        rows: &rows,
        total: app.snapshot.as_ref().map_or(0, |s| s.processes.len()),
        sort: app.sort,
        reverse: app.reverse,
        tree: app.tree,
        show_cmd: app.show_cmd,
        filter: &app.filter,
        // Windows reports every process as running and gives no thread counts.
        state_and_threads: !cfg!(windows),
    };
    process::draw_processes(frame, area, &table, &mut state);
    drop(rows);
    app.table = state;
    highlighted
}

/// Key hints as ` key action ` pairs.
fn hints<'a>(pairs: &[(&'a str, &'a str)]) -> Vec<Span<'a>> {
    pairs
        .iter()
        .flat_map(|&(key, action)| {
            [
                Span::styled(
                    format!(" {key} "),
                    Style::new().fg(VIOLET).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{action} "), DIM),
            ]
        })
        .collect()
}

/// `hidden` is the number of panels that layout collapsed in this frame.
fn draw_footer(frame: &mut Frame, area: Rect, app: &App, hidden: usize) {
    if let Some(status) = &app.status {
        let line = Line::from(vec![Span::raw(" "), Span::styled(status.clone(), PINK)]);
        frame.render_widget(line, area);
        return;
    }
    let line = match &app.mode {
        Mode::Filter => Line::from(vec![
            Span::styled(" / ", Style::new().fg(PINK).add_modifier(Modifier::BOLD)),
            Span::raw(app.filter.clone()),
            Span::styled("█", PINK),
            Span::styled("   enter keep · esc clear", DIM),
        ]),
        Mode::Normal => {
            let pause = if app.paused { "resume" } else { "pause" };
            let mut spans = hints(&[
                ("q", "quit"),
                ("?", "help"),
                ("s", "sort"),
                ("/", "filter"),
                ("space", pause),
                ("+/-", "interval"),
            ]);
            if hidden > 0 {
                spans.push(Span::styled(format!("· {hidden} hidden"), DIM));
            }
            Line::from(spans)
        }
        Mode::Help => Line::from(hints(&[("esc", "close help")])),
        Mode::ConfirmKill(_) => Line::from(hints(&[("y", "TERM"), ("f", "KILL"), ("n", "cancel")])),
    };
    frame.render_widget(line, area);
}

/// Renders `draw` into a test terminal and returns the screen as text, one line for each row.
#[cfg(test)]
pub(crate) fn render_to_string(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> String {
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(draw).unwrap();
    let buffer = terminal.backend().buffer();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Event;
    use crate::metrics::{HostInfo, ProcInfo};
    use crate::net::NetIface;
    use crate::panel::PanelSet;
    use crate::process::KillTarget;
    use crate::sensors::{Battery, BatteryState, Sensors, Temp};
    use crate::storage::{Storage, Volume};
    use crate::zfs::ZfsArc;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};

    #[test]
    fn many_cores_spread_over_more_columns() {
        assert_eq!(core_columns(4), 1);
        assert_eq!(core_columns(10), 2);
        assert_eq!(core_columns(64), 6);
    }

    #[test]
    fn core_grid_adds_columns_when_the_panel_is_short() {
        let inner = |width, height| Rect::new(0, 0, width, height);
        assert_eq!(core_grid(8, inner(78, 2)), 4);
        assert_eq!(core_grid(8, inner(33, 4)), 2);
        assert_eq!(core_grid(16, inner(78, 2)), 5, "the width limit");
        assert_eq!(core_grid(0, inner(78, 2)), 1);
        assert_eq!(core_grid(8, inner(0, 0)), 1, "never 0 columns");
        assert_eq!(core_grid(4, inner(78, 10)), 1, "tall: today's choice");
    }

    #[test]
    fn chart_points_end_at_the_right_edge() {
        let mut h = History::new(HISTORY_LEN);
        h.push(10.0);
        h.push(20.0);
        let points = chart_points(&h);
        assert_eq!(points.last(), Some(&((HISTORY_LEN - 1) as f64, 20.0)));
    }

    #[test]
    fn core_line_fits_its_width() {
        for pct in [0.0, 37.0, 100.0] {
            let line = core_line(3, pct, 30);
            assert_eq!(line.width(), 30, "pct {pct}");
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        render_to_string(width, height, |frame| draw(frame, app))
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle(Event::Key(KeyEvent::from(code)));
    }

    fn app_with(snapshot: Snapshot) -> App {
        let host = HostInfo {
            hostname: "testbox".into(),
            ..HostInfo::default()
        };
        let mut app = App::new(host, Arc::new(AtomicU64::new(1_000)));
        app.handle(Event::Metrics(snapshot));
        app
    }

    fn sample_app() -> App {
        app_with(Snapshot {
            cpus: vec![12.0, 55.0, 91.0, 3.0, 40.0, 70.0, 20.0, 99.0],
            cpu_total: 48.0,
            mem_used: 6 << 30,
            mem_total: 16 << 30,
            processes: vec![ProcInfo {
                pid: 4242,
                name: "cargo".into(),
                cpu: 87.5,
                memory: 300 << 20,
                ..ProcInfo::default()
            }],
            ..Snapshot::default()
        })
    }

    /// The ARC of worked example 1: no L2ARC, so the panel wants 6 lines.
    fn arc() -> ZfsArc {
        ZfsArc {
            size: 9 << 30,
            target: 10 << 30,
            min: 1 << 30,
            max: 16 << 30,
            mru: 5 << 30,
            mfu: 3 << 30,
            data: 6 << 30,
            metadata: 2 << 30,
            compress_ratio: Some(2.42),
            hit_pct: Some(90.0),
            iohit_pct: Some(0.0),
            demand_data_pct: Some(90.0),
            demand_meta_pct: Some(90.0),
            hit_since_boot: false,
            l2: None,
        }
    }

    fn volumes(n: usize) -> Storage {
        let volumes = [
            "/", "/home", "/boot", "/srv", "/var", "/opt", "/tmp", "/data",
        ]
        .into_iter()
        .take(n)
        .map(|mount| Volume {
            mount: mount.into(),
            device: format!("dev{mount}"),
            file_system: "ext4".into(),
            total: 100 << 30,
            available: 50 << 30,
            io_group: format!("dev{mount}"),
            io_known: true,
            read_rate: 0,
            write_rate: 0,
        })
        .collect();
        Storage {
            volumes,
            io_known: true,
            ..Storage::default()
        }
    }

    fn temps(n: usize) -> Vec<Temp> {
        (0..n)
            .map(|i| Temp {
                label: format!("temp{i}"),
                celsius: 70.0 - i as f32,
                critical: None,
            })
            .collect()
    }

    /// Ten processes named `worker-0` to `worker-9`, so a test can count the table rows.
    fn workers() -> Vec<ProcInfo> {
        (0..10)
            .map(|i| ProcInfo {
                pid: 100 + i,
                ident: 5_000,
                name: format!("worker-{i}").into(),
                cpu: i as f32,
                ..ProcInfo::default()
            })
            .collect()
    }

    /// The data of worked example 1 in design §6.10, with `cores` cores.
    fn example_app(cores: usize, zfs: bool) -> App {
        app_with(Snapshot {
            // 11%, 22%, … so each core's line can be found on the screen.
            cpus: (1..=cores).map(|i| (i * 11 % 100) as f32).collect(),
            cpu_total: 30.0,
            mem_used: 6 << 30,
            mem_total: 16 << 30,
            processes: workers(),
            interfaces: vec![NetIface {
                name: "en0".into(),
                ..NetIface::default()
            }],
            storage: volumes(3),
            zfs: zfs.then(arc),
            sensors: Sensors {
                temps: temps(10),
                battery: None,
            },
            ..Snapshot::default()
        })
    }

    fn process_rows(screen: &str) -> usize {
        screen.lines().filter(|l| l.contains("worker-")).count()
    }

    #[test]
    fn full_frame_shows_every_panel() {
        let mut app = app_with(Snapshot {
            processes: workers(),
            storage: volumes(3),
            zfs: Some(arc()),
            sensors: Sensors {
                temps: temps(3),
                battery: Some(Battery {
                    percent: 87.0,
                    state: BatteryState::Charging,
                }),
            },
            ..sample_app().snapshot.unwrap()
        });
        let screen = render(&mut app, 110, 40);
        for text in [
            "rtop",
            "testbox",
            "cpu 48%",
            "history",
            "memory 38%",
            "arc 9.0 GiB",
            "network",
            "storage 3/3",
            "zfs arc 9.0 GiB",
            "sensors",
            "bat",
            "processes 10/10",
            "worker-9",
            "quit",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        assert!(!screen.contains("hidden"), "nothing collapses:\n{screen}");
    }

    #[test]
    fn old_panels_fill_the_space_without_new_data() {
        let screen = render(&mut sample_app(), 110, 32);
        for text in [
            "cpu 48%",
            "history",
            "memory 38%",
            "network",
            "processes 1/1",
            "cargo",
            "4242",
            "87.5",
            "300.0 MiB",
            "quit",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        for text in ["zfs", "storage", "sensors", "arc", "hidden"] {
            assert!(!screen.contains(text), "unexpected {text:?} in:\n{screen}");
        }
        let last_body_row = screen.lines().nth(30).unwrap();
        assert!(
            last_body_row.starts_with('╰'),
            "processes reach the footer:\n{screen}"
        );
    }

    #[test]
    fn worked_example_at_80_by_24() {
        let mut app = example_app(8, true);
        let screen = render(&mut app, 80, 24);
        for text in ["zfs arc", "storage", "memory", "network", "processes"] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        for text in ["sensors", "history"] {
            assert!(!screen.contains(text), "unexpected {text:?} in:\n{screen}");
        }
        assert!(
            screen.lines().last().unwrap().contains("2 hidden"),
            "footer:\n{screen}"
        );
        assert_eq!(process_rows(&screen), 6, "\n{screen}");
        for pct in (1..=8).map(|i| i * 11) {
            assert!(
                screen.contains(&format!(" {pct}%")),
                "core at {pct}% missing:\n{screen}"
            );
        }
    }

    #[test]
    fn the_drawn_set_leaves_out_collapsed_and_hidden_panels() {
        let drawn = Arc::new(AtomicU16::new(PanelSet::ALL.bits()));
        let mut app = example_app(8, true).with_drawn_panels(Arc::clone(&drawn));
        press(&mut app, KeyCode::Char('3'));
        render(&mut app, 80, 24);
        let set = PanelSet::from_bits(drawn.load(Ordering::Relaxed));
        let expected: PanelSet = [
            PanelId::Cpu,
            PanelId::Network,
            PanelId::Storage,
            PanelId::Zfs,
            PanelId::Processes,
        ]
        .into_iter()
        .collect();
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>(),
            "history collapsed, memory hidden by the user"
        );

        render(&mut app, 110, 40);
        let set = PanelSet::from_bits(drawn.load(Ordering::Relaxed));
        assert!(set.contains(PanelId::Sensors), "room again: drawn again");
    }

    #[test]
    fn the_drawn_set_stays_whole_until_the_first_snapshot() {
        let drawn = Arc::new(AtomicU16::new(PanelSet::ALL.bits()));
        let host = HostInfo::default();
        let mut app =
            App::new(host, Arc::new(AtomicU64::new(1_000))).with_drawn_panels(Arc::clone(&drawn));
        render(&mut app, 80, 24);
        assert_eq!(
            PanelSet::from_bits(drawn.load(Ordering::Relaxed)),
            PanelSet::ALL,
            "the first ticks discover everything"
        );
    }

    #[test]
    fn small_screen_matrix_keeps_processes_and_memory() {
        for cores in [4, 8, 16] {
            for zfs in [false, true] {
                let mut app = example_app(cores, zfs);
                let screen = render(&mut app, 80, 24);
                let case = format!("{cores} cores, zfs {zfs}");
                assert!(process_rows(&screen) >= 6, "{case}:\n{screen}");
                if screen.contains("storage") {
                    assert!(screen.contains("memory"), "{case}:\n{screen}");
                }
            }
        }
    }

    #[test]
    fn hidden_history_is_not_drawn() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('2'));
        let screen = render(&mut app, 110, 32);
        assert!(!screen.contains("history"), "\n{screen}");
        assert!(screen.contains("cpu 48%"), "\n{screen}");
        assert!(!screen.contains("hidden"), "a user choice is not counted");
    }

    #[test]
    fn interfaces_replace_the_network_totals() {
        let mut app = example_app(8, false);
        press(&mut app, KeyCode::Char('n'));
        let screen = render(&mut app, 110, 40);
        assert!(screen.contains("network · 1 interface"), "\n{screen}");
        assert!(screen.contains("en0"), "\n{screen}");
    }

    #[test]
    fn fahrenheit_reaches_the_sensors_panel() {
        let mut app = example_app(8, false);
        app.fahrenheit = true;
        let screen = render(&mut app, 110, 40);
        assert!(screen.contains("°F"), "\n{screen}");
        assert!(!screen.contains("°C"), "\n{screen}");
    }

    #[test]
    fn help_and_kill_prompt_overlays() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('?'));
        let screen = render(&mut app, 100, 40);
        assert!(screen.contains("esc closes"), "\n{screen}");

        let mut app = sample_app();
        render(&mut app, 100, 40);
        press(&mut app, KeyCode::Char('x'));
        let screen = render(&mut app, 100, 40);
        assert!(screen.contains("y TERM"), "\n{screen}");
        assert!(screen.contains("4242 cargo"), "\n{screen}");
    }

    #[test]
    fn highlighted_follows_the_selected_row() {
        let mut app = example_app(8, false);
        render(&mut app, 110, 40);
        assert_eq!(
            app.highlighted,
            Some(KillTarget {
                pid: 109,
                ident: 5_000,
                name: "worker-9".into()
            }),
            "busiest first"
        );
        press(&mut app, KeyCode::Char('8'));
        render(&mut app, 110, 40);
        assert_eq!(app.highlighted, None, "no table, no target");
    }

    #[test]
    fn status_replaces_the_footer_hints() {
        let mut app = sample_app();
        app.status = Some("sent TERM to 4242 cargo".into());
        let screen = render(&mut app, 110, 32);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("sent TERM to 4242 cargo"), "{footer}");
        assert!(!footer.contains("quit"), "{footer}");
    }

    /// The cost of one full frame with a large process list. Run it in release:
    /// `cargo test --release draw_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a benchmark, not a check"]
    fn draw_cost() {
        use ratatui::{Terminal, backend::TestBackend};
        use std::time::Instant;

        const PROCESSES: u32 = 2_000;
        const FRAMES: u32 = 200;
        let processes = (0..PROCESSES)
            .map(|i| ProcInfo {
                pid: 100 + i,
                parent: Some(1),
                ident: u64::from(i),
                name: format!("worker-{i}").into(),
                cmd: format!("/usr/bin/worker-{i} --serve --port {i}").into(),
                user: Some("alice".into()),
                threads: Some(4),
                cpu: (i % 100) as f32,
                memory: u64::from(i) << 20,
                read_rate: u64::from(i % 7) * 1024,
                write_rate: 0,
                ..ProcInfo::default()
            })
            .collect();
        let mut app = example_app(8, false);
        app.handle(Event::Metrics(Snapshot {
            processes,
            ..app.snapshot.clone().unwrap()
        }));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let start = Instant::now();
        for _ in 0..FRAMES {
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
        let mean = start.elapsed() / FRAMES;
        println!(
            "draw_cost: {PROCESSES} processes, 120x40, {FRAMES} frames, mean {mean:?} per frame"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let modes = [
            Mode::Normal,
            Mode::Filter,
            Mode::Help,
            Mode::ConfirmKill(KillTarget {
                pid: 4242,
                ident: 0,
                name: "cargo".into(),
            }),
        ];
        for mode in modes {
            for (w, h) in [(1, 1), (10, 3), (30, 8), (200, 6), (80, 24), (40, 12)] {
                let mut app = example_app(16, true);
                app.mode = mode.clone();
                app.net_detail = true;
                render(&mut app, w, h);
                let mut app = sample_app();
                app.mode = mode.clone();
                render(&mut app, w, h);
            }
        }
    }
}
