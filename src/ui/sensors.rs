//! Sensors panel drawing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};

use super::{DIM, FAINT, heat, panel};
use crate::format::meter;
use crate::sensors::{Battery, BatteryState, Sensors, Temp};

pub const MIN_HEIGHT: u16 = 3;
pub const MIN_WIDTH: u16 = 22;
pub const MAX_ROWS: u16 = 6;

/// The battery line shows its state text only when the bar keeps at least this many cells.
const MIN_BAR: usize = 5;

/// 2 borders + 1 if a battery + min(temps, MAX_ROWS).
pub fn want_height(sensors: &Sensors) -> u16 {
    let temps = sensors.temps.len().min(usize::from(MAX_ROWS)) as u16;
    2 + u16::from(sensors.battery.is_some()) + temps
}

/// `fahrenheit` changes only the displayed unit.
pub fn draw_sensors(frame: &mut Frame, area: Rect, sensors: &Sensors, fahrenheit: bool) {
    let title = match sensors.temps.first() {
        Some(hottest) => format!(" sensors · max {} ", degrees(hottest.celsius, fahrenheit)),
        None => " sensors ".to_owned(),
    };
    let block = panel(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut rows = inner.rows();
    if let Some(battery) = &sensors.battery
        && let Some(row) = rows.next()
    {
        frame.render_widget(battery_line(battery, usize::from(row.width)), row);
    }
    for (row, temp) in rows.zip(sensors.temps.iter().take(usize::from(MAX_ROWS))) {
        draw_temp(frame, row, temp, fahrenheit);
    }
}

/// `bat {meter} {pct}% {state}`, exactly `width` cells wide when it fits.
fn battery_line(battery: &Battery, width: usize) -> Line<'static> {
    let label = "bat ";
    let pct = format!(" {:.0}%", battery.percent);
    let state = format!(" {}", state_text(battery.state));
    let fixed = label.len() + pct.len();
    let state = if width >= fixed + state.len() + MIN_BAR {
        state
    } else {
        String::new()
    };
    let bar_width = width.saturating_sub(fixed + state.len());
    let ratio = f64::from(battery.percent) / 100.0;
    let bar = meter(ratio, bar_width);
    let rest = bar_width.saturating_sub(bar.chars().count());
    Line::from(vec![
        Span::styled(label, DIM),
        // A full battery is calm; a low battery is the hot case.
        Span::styled(bar, heat(100.0 - f64::from(battery.percent))),
        Span::styled("·".repeat(rest), FAINT),
        Span::raw(pct),
        Span::styled(state, DIM),
    ])
}

fn state_text(state: BatteryState) -> &'static str {
    match state {
        BatteryState::Charging => "charging",
        BatteryState::Discharging => "discharging",
        BatteryState::Full => "full",
        BatteryState::NotCharging => "not charging",
        BatteryState::Unknown => "unknown",
    }
}

/// The label on the left and the value on the right, coloured by how near it is to critical.
fn draw_temp(frame: &mut Frame, row: Rect, temp: &Temp, fahrenheit: bool) {
    let value = degrees(temp.celsius, fahrenheit);
    let value_width = value.chars().count() as u16;
    let [label_area, value_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(value_width)])
            .spacing(1)
            .areas(row);
    let level = match temp.critical {
        Some(critical) => temp.celsius / critical * 100.0,
        None => temp.celsius,
    };
    frame.render_widget(Line::raw(temp.label.clone()), label_area);
    frame.render_widget(
        Line::styled(value, heat(f64::from(level))).right_aligned(),
        value_area,
    );
}

/// One decimal place, in the chosen unit, for example `51.8°C`.
fn degrees(celsius: f32, fahrenheit: bool) -> String {
    if fahrenheit {
        format!("{:.1}°F", celsius * 9.0 / 5.0 + 32.0)
    } else {
        format!("{celsius:.1}°C")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::{Battery, BatteryState, Temp};
    use crate::ui::render_to_string;

    fn temp(label: &str, celsius: f32, critical: Option<f32>) -> Temp {
        Temp {
            label: label.into(),
            celsius,
            critical,
        }
    }

    fn sample() -> Sensors {
        Sensors {
            temps: vec![
                temp("cpu die", 100.0, Some(105.0)),
                temp("gpu", 61.5, None),
                temp("nvme composite", 38.0, Some(84.0)),
            ],
            battery: Some(Battery {
                percent: 87.0,
                state: BatteryState::Charging,
            }),
        }
    }

    fn render(width: u16, height: u16, sensors: &Sensors, fahrenheit: bool) -> String {
        render_to_string(width, height, |frame| {
            draw_sensors(frame, frame.area(), sensors, fahrenheit)
        })
    }

    #[test]
    fn want_height_counts_battery_and_capped_temps() {
        assert_eq!(want_height(&Sensors::default()), 2);
        assert_eq!(want_height(&sample()), 6);
        let many = Sensors {
            temps: (0..10)
                .map(|i| temp(&format!("t{i}"), 40.0, None))
                .collect(),
            battery: None,
        };
        assert_eq!(want_height(&many), 2 + MAX_ROWS);
    }

    #[test]
    fn shows_battery_and_each_temperature_in_celsius() {
        let sensors = sample();
        let screen = render(40, want_height(&sensors), &sensors, false);
        for text in [
            "sensors",
            "max 100.0°C",
            "bat",
            "87%",
            "charging",
            "cpu die",
            "gpu",
            "nvme composite",
            "61.5°C",
            "°C",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        assert!(!screen.contains("°F"), "{screen}");
    }

    #[test]
    fn fahrenheit_changes_only_the_unit() {
        let sensors = sample();
        let screen = render(40, want_height(&sensors), &sensors, true);
        for text in ["°F", "212", "max 212.0°F", "cpu die"] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        assert!(!screen.contains("°C"), "{screen}");
    }

    #[test]
    fn battery_line_comes_first() {
        let sensors = sample();
        let screen = render(40, want_height(&sensors), &sensors, false);
        let lines: Vec<&str> = screen.lines().collect();
        assert!(lines[1].contains("bat"), "{screen}");
        assert!(lines[2].contains("cpu die"), "{screen}");
    }

    #[test]
    fn title_has_no_max_without_temperatures() {
        let sensors = Sensors {
            temps: Vec::new(),
            battery: Some(Battery {
                percent: 50.0,
                state: BatteryState::Discharging,
            }),
        };
        let screen = render(40, want_height(&sensors), &sensors, false);
        assert!(screen.contains(" sensors "), "{screen}");
        assert!(!screen.contains("max"), "{screen}");
        assert!(screen.contains("50%"), "{screen}");
    }

    #[test]
    fn narrow_panel_keeps_battery_percent_and_drops_state() {
        let sensors = sample();
        let screen = render(MIN_WIDTH, want_height(&sensors), &sensors, false);
        assert!(screen.contains("bat"), "{screen}");
        assert!(screen.contains("87%"), "{screen}");
        assert!(screen.contains("100.0°C"), "{screen}");
        assert!(!screen.contains("charging"), "{screen}");
    }

    #[test]
    fn short_panel_shows_the_rows_that_fit() {
        let sensors = sample();
        let screen = render(40, 4, &sensors, false);
        assert!(screen.contains("bat"), "{screen}");
        assert!(screen.contains("cpu die"), "{screen}");
        assert!(!screen.contains("gpu"), "{screen}");
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let sensors = sample();
        for (w, h) in [(1, 1), (200, 2), (10, 3), (2, 10)] {
            render(w, h, &sensors, false);
            render(w, h, &Sensors::default(), true);
        }
        render_to_string(10, 10, |frame| {
            draw_sensors(frame, Rect::new(0, 0, 0, 5), &sensors, false);
            draw_sensors(frame, Rect::new(0, 0, 5, 0), &sensors, false);
        });
    }
}
