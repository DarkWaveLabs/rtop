//! Help overlay: the key table from `crate::help::KEYMAP` over the panels.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use super::{PINK, VIOLET, panel};
use crate::help::{KEYMAP, KeySection};

const TITLE: &str = " keys · esc closes ";
/// The overlay uses two columns of sections from this width.
const TWO_COLUMNS_FROM: u16 = 76;
/// The overlay never grows wider than this, so it stays a box over the panels.
const MAX_WIDTH: u16 = 84;
const COLUMN_GAP: u16 = 2;
/// Spaces between the widest keys text of a column and its actions.
const KEY_GAP: usize = 2;

/// Draws a centred overlay over `area` (the full frame). Clears the cells under it.
/// Two columns of sections when the overlay is 76 or more wide, else one column.
/// Title: " keys · esc closes ". Cuts off what does not fit; never panics.
pub fn draw_help(frame: &mut Frame, area: Rect) {
    let two_columns = area.width.min(MAX_WIDTH) >= TWO_COLUMNS_FROM;
    let columns: Vec<Vec<Line>> = if two_columns {
        let (left, right) = KEYMAP.split_at(balanced_split(KEYMAP));
        vec![column_lines(left), column_lines(right)]
    } else {
        vec![column_lines(KEYMAP)]
    };

    let natural_width = if two_columns {
        MAX_WIDTH
    } else {
        let widest = columns[0].iter().map(Line::width).max().unwrap_or(0);
        to_u16(widest.max(TITLE.chars().count())).saturating_add(2)
    };
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(0);
    let width = area.width.min(natural_width);
    let height = area.height.min(to_u16(tallest).saturating_add(2));
    let overlay = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, overlay);
    let block = panel(TITLE);
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    let areas = Layout::horizontal(vec![Constraint::Fill(1); columns.len()])
        .spacing(COLUMN_GAP)
        .split(inner);
    for (lines, column) in columns.into_iter().zip(areas.iter()) {
        frame.render_widget(Paragraph::new(lines), *column);
    }
}

/// The number of sections in the left column that makes the taller column as short as possible.
fn balanced_split(sections: &[KeySection]) -> usize {
    (1..sections.len())
        .min_by_key(|&at| {
            let (left, right) = sections.split_at(at);
            column_height(left).max(column_height(right))
        })
        .unwrap_or(sections.len())
}

/// A title line and one line for each key in each section, with a blank line between sections.
fn column_height(sections: &[KeySection]) -> usize {
    let lines: usize = sections.iter().map(|s| s.keys.len() + 1).sum();
    lines + sections.len().saturating_sub(1)
}

fn column_lines(sections: &[KeySection]) -> Vec<Line<'static>> {
    let keys_width = sections
        .iter()
        .flat_map(|s| s.keys)
        .map(|k| Span::raw(k.keys).width())
        .max()
        .unwrap_or(0)
        + KEY_GAP;
    let title_style = Style::new().fg(PINK).add_modifier(Modifier::BOLD);
    let key_style = Style::new().fg(VIOLET).add_modifier(Modifier::BOLD);

    let mut lines = Vec::with_capacity(column_height(sections));
    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::styled(section.title, title_style));
        for key in section.keys {
            let pad = keys_width.saturating_sub(Span::raw(key.keys).width());
            lines.push(Line::from(vec![
                Span::styled(key.keys, key_style),
                Span::raw(" ".repeat(pad)),
                Span::raw(key.action),
            ]));
        }
    }
    lines
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(width: u16, height: u16) -> String {
        super::super::render_to_string(width, height, |frame| draw_help(frame, frame.area()))
    }

    #[test]
    fn large_overlay_shows_title_and_keys() {
        let screen = render(100, 40);
        for text in ["keys", "esc closes", "kill", "tree", "zfs"] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
    }

    #[test]
    fn large_overlay_shows_every_section_and_action() {
        let screen = render(100, 40);
        for section in KEYMAP {
            assert!(
                screen.contains(section.title),
                "missing {:?}",
                section.title
            );
            for key in section.keys {
                assert!(screen.contains(key.action), "missing {:?}", key.action);
            }
        }
    }

    #[test]
    fn wide_overlay_puts_sections_side_by_side() {
        let screen = render(100, 40);
        let row_of = |text: &str| {
            screen
                .lines()
                .position(|l| l.contains(text))
                .unwrap_or_else(|| panic!("missing {text:?} in:\n{screen}"))
        };
        assert_eq!(row_of("General"), row_of("Panels"), "\n{screen}");
    }

    #[test]
    fn narrow_overlay_uses_one_column() {
        let screen = render(60, 60);
        let row_of = |text: &str| screen.lines().position(|l| l.contains(text)).unwrap();
        assert!(row_of("General") < row_of("Processes"));
        assert!(row_of("Processes") < row_of("Panels"), "\n{screen}");
        assert!(
            row_of("show or hide zfs") < row_of("Filter, help"),
            "\n{screen}"
        );
    }

    #[test]
    fn overlay_is_centred_and_clears_what_is_under_it() {
        use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| {
                let fill = vec!["#".repeat(100); 40].join("\n");
                frame.render_widget(Paragraph::new(fill), frame.area());
                draw_help(frame, frame.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(50, 20)].symbol(), " ", "centre is not cleared");
        assert_eq!(buffer[(0, 0)].symbol(), "#", "corner was drawn over");
        assert_eq!(buffer[(99, 39)].symbol(), "#", "corner was drawn over");
    }

    #[test]
    fn standard_terminal_shows_the_title() {
        let screen = render(80, 24);
        assert!(screen.contains("keys · esc closes"), "\n{screen}");
    }

    #[test]
    fn small_terminals_do_not_panic() {
        render(40, 12);
        for (w, h) in [(1, 1), (1, 40), (100, 1), (2, 2), (75, 3), (76, 24)] {
            render(w, h);
        }
    }

    #[test]
    fn empty_and_offset_areas_do_not_panic() {
        super::super::render_to_string(30, 10, |frame| {
            draw_help(frame, Rect::new(0, 0, 0, 0));
            draw_help(frame, Rect::new(5, 5, 0, 3));
            draw_help(frame, Rect::new(5, 5, 3, 0));
            draw_help(frame, Rect::new(10, 5, 20, 5));
            draw_help(frame, Rect::new(29, 9, 1, 1));
        });
    }
}
