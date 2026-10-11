//! Drawing the monitor screen.

use std::time::Duration;

use jackerymon::control::Setting;
use jackerymon::status::BatteryProtection;
use jackerymon::status::BatteryState;
use jackerymon::status::ChargeSpeed;
use jackerymon::status::LightMode;
use jackerymon::status::OffTimer;
use jackerymon::status::ScreenMode;
use jackerymon::status::Status;
use jackerymon::status::TemperatureAlarm;
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::symbols::Marker;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Axis;
use ratatui::widgets::Block;
use ratatui::widgets::Chart;
use ratatui::widgets::Clear;
use ratatui::widgets::Dataset;
use ratatui::widgets::Gauge;
use ratatui::widgets::GraphType;
use ratatui::widgets::Paragraph;
use tokio::time::Instant;

use super::menu::Control;
use super::menu::Menu;
use super::monitor::ControlState;
use super::monitor::HISTORY;
use super::monitor::Link;
use super::monitor::Monitor;

/// Data older than this is shown as stale.
pub(super) const STALE_AFTER: Duration = Duration::from_secs(30);
const PANEL_HEIGHT: u16 = 9;
/// Fits the longest label, "Temp alarm", plus a space.
const LABEL_WIDTH: usize = 11;
const MINUTES_PER_HOUR: u32 = 60;
const SECONDS_PER_MINUTE: u64 = 60;
/// Chart y axis never shrinks below this, so idle noise stays flat.
const MIN_CHART_WATTS: f64 = 10.0;
/// Headroom above the highest reading.
const CHART_HEADROOM: f64 = 1.2;
const INPUT_COLOR: Color = Color::Green;
const OUTPUT_COLOR: Color = Color::Yellow;
const ALERT_COLOR: Color = Color::Red;
const MISSING: &str = "—";
const MENU_WIDTH: u16 = 44;
/// Borders above and below the menu rows.
const MENU_BORDER_HEIGHT: u16 = 2;
const MENU_VALUE_WIDTH: usize = 20;

pub(super) fn draw(
    frame: &mut Frame,
    monitor: &Monitor,
    menu: Menu,
    controls: &[Control],
    now: Instant,
) {
    let [header, panels, chart, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(PANEL_HEIGHT),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(header_line(monitor, now), header);
    let status = monitor.latest.as_ref().map(|reading| &reading.status);
    let [battery, power, unit] = Layout::horizontal([Constraint::Fill(1); 3]).areas(panels);
    draw_battery(frame, battery, status);
    frame.render_widget(power_panel(status), power);
    frame.render_widget(unit_panel(status), unit);
    draw_chart(frame, chart, monitor, now);
    frame.render_widget(footer_line(monitor), footer);
    draw_menu(frame, menu, controls, status);
}

fn draw_menu(frame: &mut Frame, menu: Menu, controls: &[Control], status: Option<&Status>) {
    let selected = |line: Line<'static>, chosen: bool| {
        if chosen { line.reversed() } else { line }
    };
    let (title, lines) = match menu {
        Menu::Closed => return,
        Menu::Controls { row } => (
            " Settings ".to_owned(),
            controls
                .iter()
                .enumerate()
                .map(|(index, control)| {
                    let value = control
                        .current(status)
                        .map(|choice| setting_value_text(control.choices[choice]));
                    selected(menu_row(control.label, value), index == row)
                })
                .collect(),
        ),
        Menu::Choices { row, choice } => {
            let control = &controls[row];
            let current = control.current(status);
            (
                format!(" {} ", control.label),
                control
                    .choices
                    .iter()
                    .enumerate()
                    .map(|(index, setting)| {
                        let marker = if current == Some(index) { "● " } else { "  " };
                        let text = format!("{marker}{}", setting_value_text(*setting));
                        selected(Line::from(text), index == choice)
                    })
                    .collect(),
            )
        }
        Menu::Confirm { row, choice } => (
            " Confirm ".to_owned(),
            vec![
                Line::from(format!(
                    "Turn {} {}?",
                    controls[row].label,
                    setting_value_text(controls[row].choices[choice])
                )),
                Line::from(
                    controls[row].choices[choice]
                        .warning()
                        .unwrap_or_default()
                        .to_owned(),
                )
                .fg(ALERT_COLOR),
                Line::from("y yes   n no"),
            ],
        ),
    };
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX) + MENU_BORDER_HEIGHT;
    let area = frame
        .area()
        .centered(Constraint::Length(MENU_WIDTH), Constraint::Length(height));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

fn menu_row(label: &str, value: Option<String>) -> Line<'static> {
    Line::from(format!(
        "{label:<MENU_VALUE_WIDTH$}{}",
        value.unwrap_or_else(|| MISSING.to_owned())
    ))
}

/// The value part of a setting, as the panels show it.
fn setting_value_text(setting: Setting) -> String {
    match setting {
        Setting::AcOutput(on)
        | Setting::DcOutput(on)
        | Setting::EmergencyCharge(on)
        | Setting::Ups(on) => on_off(on),
        Setting::Light(mode) => light_text(mode),
        Setting::Screen(mode) => screen_text(mode),
        Setting::AutoOff(timer) | Setting::EnergySaving(timer) => timer_text(timer),
        Setting::ChargeSpeed(speed) => match speed {
            ChargeSpeed::Fast => "fast".to_owned(),
            ChargeSpeed::Quiet => "quiet".to_owned(),
            ChargeSpeed::Custom => "custom".to_owned(),
            ChargeSpeed::Unknown(value) => format!("unknown ({value})"),
        },
        Setting::BatteryProtection(protection) => match protection {
            BatteryProtection::Full => "full".to_owned(),
            BatteryProtection::Eco => "eco".to_owned(),
            BatteryProtection::Unknown(value) => format!("unknown ({value})"),
        },
    }
}

fn header_line(monitor: &Monitor, now: Instant) -> Line<'static> {
    let link = match &monitor.link {
        Link::Searching => Span::raw("searching…"),
        Link::Connected => Span::styled("● connected", INPUT_COLOR),
        Link::Lost { error, consecutive } => Span::styled(
            format!("reconnecting ({consecutive}): {error}"),
            ALERT_COLOR,
        ),
    };
    let age = monitor.latest.as_ref().map_or_else(
        || Span::raw("no data yet"),
        |reading| {
            let age = now.duration_since(reading.received);
            let text = format!("updated {} ago", duration_text(age));
            if age > STALE_AFTER {
                Span::styled(text, ALERT_COLOR)
            } else {
                Span::raw(text)
            }
        },
    );
    let mut spans = vec![
        Span::raw(format!("Jackery {}", monitor.serial)).bold(),
        Span::raw("  "),
        link,
        Span::raw("  "),
        age,
    ];
    if let Some(invalid) = &monitor.invalid {
        spans.push(Span::styled(
            format!("  bad status: {invalid}"),
            ALERT_COLOR,
        ));
    }
    Line::from(spans)
}

fn footer_line(monitor: &Monitor) -> Line<'static> {
    let mut spans = vec![
        Span::raw(format!(
            "q quit   m settings   samples {}   dropouts {}",
            monitor.samples, monitor.transport_failures
        ))
        .dim(),
    ];
    match &monitor.control {
        None => {}
        Some(ControlState::Pending(setting)) => {
            spans.push(Span::raw(format!("   applying {setting}…")));
        }
        Some(ControlState::Applied(setting)) => {
            spans.push(Span::styled(format!("   applied {setting}"), INPUT_COLOR));
        }
        Some(ControlState::Failed(error)) => {
            spans.push(Span::styled(format!("   {error}"), ALERT_COLOR));
        }
    }
    Line::from(spans)
}

fn draw_battery(frame: &mut Frame, area: Rect, status: Option<&Status>) {
    let block = Block::bordered().title(" Battery ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [gauge, rows] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let percent = status.and_then(|status| status.battery_percent);
    frame.render_widget(
        Gauge::default()
            .percent(percent.map_or(0, |percent| u16::from(percent.min(100))))
            .label(percent.map_or_else(|| MISSING.to_owned(), |percent| format!("{percent} %")))
            .gauge_style(INPUT_COLOR)
            .use_unicode(true),
        gauge,
    );
    let field = |pick: fn(&Status) -> Option<String>| status.and_then(pick);
    frame.render_widget(
        Paragraph::new(vec![
            Line::default(),
            row(
                "Temp",
                field(|status| {
                    status
                        .battery_celsius
                        .map(|celsius| format!("{celsius:.1} °C"))
                }),
            ),
            row(
                "State",
                field(|status| status.battery_state.map(battery_state_text)),
            ),
            row("To full", field(to_full_text)),
            row(
                "To empty",
                field(|status| status.output_hours_remaining.map(hours_text)),
            ),
            alert_row(
                "Temp alarm",
                field(|status| status.temperature_alarm.map(temperature_alarm_text)),
                status
                    .and_then(|status| status.temperature_alarm)
                    .is_some_and(|alarm| alarm != TemperatureAlarm::None),
            ),
        ]),
        rows,
    );
}

fn power_panel(status: Option<&Status>) -> Paragraph<'static> {
    let field = |pick: fn(&Status) -> Option<String>| status.and_then(pick);
    Paragraph::new(vec![
        row(
            "Input",
            field(|status| status.input_watts.map(|watts| format!("{watts} W"))),
        )
        .fg(INPUT_COLOR),
        row(
            "Output",
            field(|status| status.output_watts.map(|watts| format!("{watts} W"))),
        )
        .fg(OUTPUT_COLOR),
        row(
            "AC in",
            field(|status| status.ac_input_watts.map(|watts| format!("{watts} W"))),
        ),
        row(
            "Car in",
            field(|status| status.car_input_watts.map(|watts| format!("{watts} W"))),
        ),
        row(
            "Solar in",
            field(|status| {
                status
                    .solar_input_watts
                    .map(|watts| format!("{watts:.1} W"))
            }),
        ),
        row("AC out", field(ac_output_text)),
    ])
    .block(Block::bordered().title(" Power "))
}

fn unit_panel(status: Option<&Status>) -> Paragraph<'static> {
    let field = |pick: fn(&Status) -> Option<String>| status.and_then(pick);
    let mut lines = vec![
        row("AC", field(|status| status.ac_output.map(on_off))),
        row("DC", field(|status| status.dc_output.map(on_off))),
    ];
    // Some models split DC into USB and car; the Explorer 2000 does not.
    for (label, pick) in [
        (
            "USB",
            (|status: &Status| status.usb_output) as fn(&Status) -> Option<bool>,
        ),
        ("Car", |status: &Status| status.car_output),
    ] {
        if let Some(state) = status.and_then(pick) {
            lines.push(row(label, Some(on_off(state))));
        }
    }
    let fault = status.and_then(|status| status.error_code);
    lines.extend([
        row("Light", field(|status| status.light_mode.map(light_text))),
        row("Screen", field(|status| status.screen.map(screen_text))),
        row("Auto-off", field(|status| status.auto_off.map(timer_text))),
        row(
            "Eco off",
            field(|status| status.energy_saving.map(timer_text)),
        ),
        alert_row(
            "Fault",
            fault.map(fault_text),
            fault.is_some_and(|code| code != 0),
        ),
    ]);
    Paragraph::new(lines).block(Block::bordered().title(" Unit "))
}

fn draw_chart(frame: &mut Frame, area: Rect, monitor: &Monitor, now: Instant) {
    let since = |at: Instant| -now.duration_since(at).as_secs_f64();
    let input: Vec<(f64, f64)> = monitor
        .history
        .iter()
        .map(|point| (since(point.at), point.input_watts))
        .collect();
    let output: Vec<(f64, f64)> = monitor
        .history
        .iter()
        .map(|point| (since(point.at), point.output_watts))
        .collect();
    let peak = monitor
        .history
        .iter()
        .map(|point| point.input_watts.max(point.output_watts))
        .fold(0.0, f64::max);
    let top = (peak * CHART_HEADROOM).max(MIN_CHART_WATTS).ceil();
    let window = HISTORY.as_secs_f64();
    let minutes = HISTORY.as_secs() / SECONDS_PER_MINUTE;
    let datasets = vec![
        Dataset::default()
            .name("input")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(INPUT_COLOR)
            .data(&input),
        Dataset::default()
            .name("output")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(OUTPUT_COLOR)
            .data(&output),
    ];
    let chart = Chart::new(datasets)
        .block(Block::bordered().title(format!(" Power, last {minutes} min ")))
        .x_axis(
            Axis::default()
                .bounds([-window, 0.0])
                .labels([format!("-{minutes} min"), "now".to_owned()]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, top])
                .labels(["0 W".to_owned(), format!("{top} W")]),
        );
    frame.render_widget(chart, area);
}

fn row(label: &str, value: Option<String>) -> Line<'static> {
    Line::from(format!(
        "{label:<LABEL_WIDTH$}{}",
        value.unwrap_or_else(|| MISSING.to_owned())
    ))
}

fn alert_row(label: &str, value: Option<String>, alert: bool) -> Line<'static> {
    let line = row(label, value);
    if alert {
        line.style(Style::new().fg(ALERT_COLOR))
    } else {
        line
    }
}

fn on_off(state: bool) -> String {
    if state { "on" } else { "off" }.to_owned()
}

fn hours_text(hours: f64) -> String {
    format!("{hours:.1} h")
}

/// The unit reports 0 h to full while nothing charges; that is not an
/// estimate.
fn to_full_text(status: &Status) -> Option<String> {
    if status.input_watts == Some(0) {
        None
    } else {
        status.input_hours_remaining.map(hours_text)
    }
}

fn ac_output_text(status: &Status) -> Option<String> {
    if status.ac_output? {
        Some(format!(
            "{:.1} V {} Hz",
            status.ac_output_volts?, status.ac_output_hertz?
        ))
    } else {
        Some(MISSING.to_owned())
    }
}

fn battery_state_text(state: BatteryState) -> String {
    match state {
        BatteryState::Idle => "idle".to_owned(),
        BatteryState::Charging => "charging".to_owned(),
        BatteryState::Discharging => "discharging".to_owned(),
        BatteryState::Unknown(value) => format!("unknown ({value})"),
    }
}

fn light_text(mode: LightMode) -> String {
    match mode {
        LightMode::Off => "off".to_owned(),
        LightMode::Low => "low".to_owned(),
        LightMode::High => "high".to_owned(),
        LightMode::Sos => "SOS".to_owned(),
        LightMode::Unknown(value) => format!("unknown ({value})"),
    }
}

fn screen_text(mode: ScreenMode) -> String {
    match mode {
        ScreenMode::Off => "off".to_owned(),
        ScreenMode::Brief => "2 min".to_owned(),
        ScreenMode::Steady => "2 h".to_owned(),
        ScreenMode::Unknown(value) => format!("unknown ({value})"),
    }
}

fn temperature_alarm_text(alarm: TemperatureAlarm) -> String {
    match alarm {
        TemperatureAlarm::None => "none".to_owned(),
        TemperatureAlarm::High => "high".to_owned(),
        TemperatureAlarm::Low => "low".to_owned(),
        TemperatureAlarm::Unknown(value) => format!("unknown ({value})"),
    }
}

fn timer_text(timer: OffTimer) -> String {
    match timer {
        OffTimer::Never => "never".to_owned(),
        OffTimer::Minutes(minutes) if minutes % MINUTES_PER_HOUR == 0 => {
            format!("{} h", minutes / MINUTES_PER_HOUR)
        }
        OffTimer::Minutes(minutes) => format!("{minutes} min"),
    }
}

/// The app shows code n as fault F(n-1).
fn fault_text(code: i64) -> String {
    if code == 0 {
        "none".to_owned()
    } else {
        format!("F{}", code - 1)
    }
}

fn duration_text(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < SECONDS_PER_MINUTE {
        format!("{seconds} s")
    } else {
        format!("{} min", seconds / SECONDS_PER_MINUTE)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jackerymon::poll::PollEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;
    use tokio::time::Instant;

    use super::STALE_AFTER;
    use super::draw;
    use super::fault_text;
    use super::timer_text;
    use crate::tui::menu::Menu;
    use crate::tui::menu::controls;
    use crate::tui::monitor::Monitor;
    use crate::tui::monitor::tests::sample;
    use jackerymon::status::OffTimer;

    const WIDTH: u16 = 100;
    const HEIGHT: u16 = 24;

    fn render(monitor: &Monitor, now: Instant) -> String {
        render_with(monitor, Menu::Closed, now)
    }

    fn render_with(monitor: &Monitor, menu: Menu, now: Instant) -> String {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
        terminal
            .draw(|frame| draw(frame, monitor, menu, &controls(), now))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn shows_live_explorer_2000_status() {
        let now = Instant::now();
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(PollEvent::Connected { connection: 1 }, now);
        monitor.apply(
            sample(
                1,
                json!({
                    "rb": 100, "bt": 279, "bs": 0, "ip": 0, "op": 1, "it": 0,
                    "ot": 999, "oac": 1, "odc": 0, "acov": 1209, "acohz": 60,
                    "lm": 1, "sltb": 2, "ast": 120, "pm": 720, "ec": 0, "ta": 0,
                    "cip": 0, "acpsp": 0,
                }),
            ),
            now,
        );
        let screen = render(&monitor, now + Duration::from_secs(3));
        for expected in [
            "Jackery 123456789",
            "connected",
            "updated 3 s ago",
            "100 %",
            "27.9 °C",
            "99.9 h",
            "To full    —",
            "120.9 V 60 Hz",
            "2 min",
            "12 h",
            "samples 1",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn marks_stale_and_missing_data() {
        let now = Instant::now();
        let monitor = Monitor::new("123456789".to_owned());
        let screen = render(&monitor, now);
        assert!(screen.contains("searching"));
        assert!(screen.contains("no data yet"));
        let mut monitor = monitor;
        monitor.apply(sample(1, json!({"rb": 50})), now);
        let screen = render(&monitor, now + STALE_AFTER + Duration::from_secs(60));
        assert!(screen.contains("updated 1 min ago"));
    }

    #[test]
    fn formats_settings() {
        assert_eq!(timer_text(OffTimer::Never), "never");
        assert_eq!(timer_text(OffTimer::Minutes(120)), "2 h");
        assert_eq!(timer_text(OffTimer::Minutes(90)), "90 min");
        assert_eq!(fault_text(0), "none");
        assert_eq!(fault_text(3), "F2");
    }

    #[test]
    fn menu_shows_current_values_and_confirmation() {
        let now = Instant::now();
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(sample(1, json!({"oac": 1, "lm": 2, "ast": 120})), now);
        let screen = render_with(&monitor, Menu::Controls { row: 0 }, now);
        for expected in ["Settings", "AC output", "Light", "high", "Auto-off"] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
        let row = controls()
            .iter()
            .position(|control| control.label == "Emergency charge")
            .expect("emergency charge row");
        let screen = render_with(&monitor, Menu::Confirm { row, choice: 0 }, now);
        assert!(screen.contains("Turn Emergency charge on?"));
        assert!(screen.contains("for emergencies only"));
    }
}
