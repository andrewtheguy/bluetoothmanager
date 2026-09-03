//! Rendering. Everything here reads from `App`; nothing here mutates it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};

use crate::app::{App, ConfirmKind, Field, Focus, Level, Modal, Prompt};
use crate::bluez::{Adapter, Device, PowerState, company_name, describe_uuid, rssi_bars};

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

const FG_DIM: Color = Color::DarkGray;
const FG_KEY: Color = Color::Gray;
const ACCENT: Color = Color::Blue;
const BG_DEEP: Color = Color::Rgb(16, 18, 22);

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let [lists, details] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(13)]).areas(body);

    let [adapters_area, devices_area] =
        Layout::horizontal([Constraint::Length(36), Constraint::Fill(1)]).areas(lists);

    let [adapter_detail_area, device_detail_area] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Fill(1)]).areas(details);

    draw_header(frame, header, app);
    draw_adapters(frame, adapters_area, app);
    draw_adapter_detail(frame, adapter_detail_area, app);
    draw_devices(frame, devices_area, app);
    draw_device_detail(frame, device_detail_area, app);
    draw_footer(frame, footer, app);

    match &app.modal {
        Modal::None => {}
        Modal::Help => draw_help(frame),
        Modal::Prompt(p) => draw_prompt(frame, p),
        Modal::Confirm(c) => draw_confirm(frame, c),
        Modal::Info { title, text } => draw_info(frame, title, text),
    }
}

// ---------------------------------------------------------------------- chrome

/// A bordered pane. The focused one is meant to be obvious across the room:
/// a heavy accent border and a reversed title chip, against a thin dim border
/// and plain title for everything else.
fn panel(title: impl Into<String>, focused: bool) -> Block<'static> {
    let (border_type, border_style, title_style) = if focused {
        (
            BorderType::Thick,
            Style::default().fg(ACCENT),
            Style::default().fg(BG_DEEP).bg(ACCENT).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            BorderType::Rounded,
            Style::default().fg(FG_DIM),
            Style::default().fg(FG_KEY),
        )
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(Span::styled(format!(" {} ", title.into()), title_style))
}

fn dialog(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(" bluetoothmanager ", Style::default().fg(Color::Black).bg(ACCENT)),
        Span::raw(" "),
    ];
    match app.adapter() {
        Some(a) => {
            spans.push(power_chip(a));
            spans.push(Span::raw("  "));
            if a.discovering {
                spans.push(Span::styled("scanning", Style::default().fg(Color::Yellow)));
                spans.push(Span::raw("  "));
            }
            if a.discoverable {
                spans.push(Span::styled("visible", Style::default().fg(Color::Magenta)));
                spans.push(Span::raw("  "));
            }
            let connected = a.connected_devices().count();
            if connected > 0 {
                spans.push(kv("connected", &connected.to_string()));
                spans.push(Span::raw("  "));
            }
            spans.push(kv("adapter", &format!("{} ({})", a.alias, a.id)));
        }
        None if app.first_load => spans.push(Span::styled("loading…", Style::default().fg(FG_DIM))),
        None => spans.push(Span::styled(
            " no adapter ",
            Style::default().fg(Color::Black).bg(Color::Red),
        )),
    }
    spans.push(Span::styled("  ? help", Style::default().fg(FG_DIM)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn power_chip(a: &Adapter) -> Span<'static> {
    let (text, bg) = match a.power_state {
        PowerState::OffBlocked => (" blocked ", Color::Red),
        PowerState::OffEnabling => (" powering on ", Color::Yellow),
        PowerState::OnDisabling => (" powering off ", Color::Yellow),
        _ if a.powered => (" Bluetooth on ", Color::Green),
        _ => (" Bluetooth off ", Color::Yellow),
    };
    Span::styled(text, Style::default().fg(Color::Black).bg(bg))
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    if app.status_visible() && !app.status.text.is_empty() {
        let color = match app.status.level {
            Level::Info => ACCENT,
            Level::Ok => Color::Green,
            Level::Error => Color::Red,
        };
        let mut spans = Vec::new();
        if app.status.busy {
            let i = (app.status_at.elapsed().as_millis() / 90) as usize % SPINNER.len();
            spans.push(Span::styled(
                format!(" {} ", SPINNER[i]),
                Style::default().fg(color),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(app.status.text.clone(), Style::default().fg(color)));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    // Name the pane Tab moves to, so the arrow keys are never a guess.
    let tab_hint = match app.focus {
        Focus::Adapters => "tab → devices",
        Focus::Devices => "tab → adapters",
    };
    let keys: &[(&str, &str)] = match app.focus {
        Focus::Devices => &[
            ("↵", "connect"),
            ("d", "disconnect"),
            ("s", "scan"),
            ("w", "power"),
            ("f", "forget"),
            ("t", "trust"),
            ("b", "block"),
            ("n", "rename"),
            ("v", "visible"),
            ("q", "quit"),
        ],
        Focus::Adapters => &[
            ("w", "power"),
            ("s", "scan"),
            ("v", "visible"),
            ("p", "pairable"),
            ("n", "rename"),
            ("q", "quit"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in keys {
        spans.push(Span::styled(
            (*k).to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}  "),
            Style::default().fg(FG_DIM),
        ));
    }
    spans.push(Span::styled(
        tab_hint.to_string(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// -------------------------------------------------------------------- adapters

fn draw_adapters(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Adapters;
    let block = panel(format!("Adapters ({})", app.adapters().len()), focused);

    if app.adapters().is_empty() {
        let msg = if app.first_load {
            "loading…"
        } else {
            "no Bluetooth adapters"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(FG_DIM))).block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app.adapters().iter().map(adapter_row).collect();

    let mut state = ListState::default().with_selected(Some(app.adapter_index()));
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(selection_style(focused))
            .highlight_symbol(selection_marker(focused)),
        area,
        &mut state,
    );
}

fn adapter_row(a: &Adapter) -> ListItem<'static> {
    let color = adapter_color(a);
    let head = Line::from(vec![
        Span::styled(if a.powered { "●" } else { "○" }, Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(
            format!("{:<8}", truncate(&a.id, 8)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(a.power_state.to_string(), Style::default().fg(color)),
    ]);

    // Second line: whatever is most useful to know at a glance about this
    // adapter without selecting it.
    let connected = a.connected_devices().count();
    let detail = if !a.powered {
        truncate(&a.alias, 30)
    } else if connected > 0 {
        let names: Vec<String> = a.connected_devices().map(|d| d.display_name()).collect();
        truncate(&names.join(", "), 30)
    } else if a.discovering {
        "scanning…".into()
    } else {
        format!(
            "{} known device{}",
            a.devices.len(),
            if a.devices.len() == 1 { "" } else { "s" }
        )
    };

    ListItem::new(vec![
        head,
        Line::from(Span::styled(
            format!("  {detail}"),
            Style::default().fg(FG_DIM),
        )),
    ])
}

fn draw_adapter_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Adapter", false);
    let Some(a) = app.adapter() else {
        frame.render_widget(Paragraph::new("").block(block), area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("{:<11}", "name"), Style::default().fg(FG_KEY)),
        Span::styled(a.alias.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            if a.alias == a.name || a.name.is_empty() {
                String::new()
            } else {
                format!("  (system name {})", a.name)
            },
            Style::default().fg(FG_DIM),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<11}", "power"), Style::default().fg(FG_KEY)),
        Span::styled(a.power_state.to_string(), Style::default().fg(adapter_color(a))),
    ]));
    lines.push(row("address", &format!("{} ({})", a.address, a.address_type)));

    let mut flags = Vec::new();
    flags.push(if a.discoverable {
        if a.discoverable_timeout == 0 {
            "visible".to_string()
        } else {
            format!("visible for {}", duration(a.discoverable_timeout))
        }
    } else {
        "hidden".to_string()
    });
    flags.push(if a.pairable { "pairable".into() } else { "not pairable".into() });
    if a.discovering {
        flags.push("scanning".into());
    }
    if !a.connectable && a.powered {
        flags.push("not connectable".into());
    }
    lines.push(row("flags", &flags.join(" · ")));

    if !a.roles.is_empty() {
        lines.push(row("roles", &a.roles.join(", ")));
    }

    let mut hw = Vec::new();
    if let Some(name) = company_name(a.manufacturer) {
        hw.push(name.to_string());
    } else if a.manufacturer != 0 {
        hw.push(format!("vendor {:#06x}", a.manufacturer));
    }
    if let Some(core) = core_version(a.version) {
        hw.push(format!("Bluetooth {core}"));
    }
    if !a.modalias.is_empty() {
        hw.push(a.modalias.clone());
    }
    if !hw.is_empty() {
        lines.push(row("hardware", &hw.join(" · ")));
    }

    let paired = a.devices.iter().filter(|d| d.paired).count();
    let connected = a.connected_devices().count();
    lines.push(row(
        "devices",
        &format!(
            "{} known · {} paired · {} connected",
            a.devices.len(),
            paired,
            connected
        ),
    ));

    let profiles: Vec<String> = a
        .uuids
        .iter()
        .filter_map(|u| crate::bluez::profile_name(u))
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !profiles.is_empty() {
        lines.push(row("offers", &profiles.join(", ")));
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

// --------------------------------------------------------------------- devices

fn draw_devices(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Devices;
    let title = match app.adapter() {
        Some(a) => format!("Devices on {} ({})", a.id, a.devices.len()),
        None => "Devices".to_string(),
    };
    let block = panel(title, focused);

    if app.devices().is_empty() {
        let msg = if app.first_load {
            "loading…"
        } else if app.adapter().is_none() {
            "no adapter"
        } else if !app.adapter().is_some_and(|a| a.powered) {
            "Bluetooth is off — press w to turn it on"
        } else if app.adapter().is_some_and(|a| a.discovering) {
            "scanning — devices in pairing mode will appear here"
        } else {
            "no devices known — press s to scan"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(FG_DIM))).block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app.devices().iter().map(device_row).collect();
    let mut state = ListState::default().with_selected(Some(app.device_index()));
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(selection_style(focused))
            .highlight_symbol(selection_marker(focused)),
        area,
        &mut state,
    );
}

fn device_row(d: &Device) -> ListItem<'_> {
    let name_style = if d.connected {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else if d.blocked {
        Style::default().fg(Color::Red)
    } else if d.name.is_empty() {
        Style::default().fg(FG_DIM)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![
        Span::styled(
            if d.connected { "● " } else { "  " },
            Style::default().fg(Color::Green),
        ),
        Span::raw(format!("{:<3}", d.kind().glyph())),
        Span::styled(format!("{:<26}", truncate(&d.display_name(), 26)), name_style),
    ];
    match d.rssi {
        Some(rssi) => {
            spans.extend(signal_bars(rssi));
            spans.push(Span::styled(
                format!(" {rssi:>4}  "),
                Style::default().fg(rssi_color(rssi)),
            ));
        }
        None => spans.push(Span::styled("           ", Style::default().fg(FG_DIM))),
    }
    spans.push(Span::styled(
        format!("{:<8}", if d.paired { "paired" } else { "" }),
        Style::default().fg(ACCENT),
    ));
    spans.push(Span::styled(
        format!("{:<9}", if d.blocked { "blocked" } else if d.trusted { "trusted" } else { "" }),
        Style::default().fg(if d.blocked { Color::Red } else { Color::Magenta }),
    ));
    spans.push(Span::styled(
        format!("{:<11}", d.kind().to_string()),
        Style::default().fg(FG_DIM),
    ));
    if let Some(b) = d.battery {
        spans.push(Span::styled(
            format!("🔋{b:>3}%"),
            Style::default().fg(battery_color(b)),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn draw_device_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Device", false);
    let Some(d) = app.device() else {
        frame.render_widget(Paragraph::new("").block(block), area);
        return;
    };

    let mut lines = vec![Line::from(vec![
        Span::raw(format!("{} ", d.kind().glyph())),
        Span::styled(d.display_name(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            if !d.name.is_empty() && d.name != d.alias {
                format!("  ({})", d.name)
            } else {
                String::new()
            },
            Style::default().fg(FG_DIM),
        ),
        Span::styled(
            if d.connected { "  connected" } else { "" },
            Style::default().fg(Color::Green),
        ),
    ])];
    lines.push(row(
        "address",
        &format!(
            "{} ({}{})",
            d.address,
            d.address_type,
            if d.is_le() { ", LE" } else { "" }
        ),
    ));

    let mut what = vec![d.kind().to_string()];
    if d.class != 0 {
        what.push(format!("class {:#08x}", d.class));
    }
    if d.appearance != 0 {
        what.push(format!("appearance {:#06x}", d.appearance));
    }
    if !d.icon.is_empty() {
        what.push(d.icon.clone());
    }
    lines.push(row("type", &what.join(" · ")));

    let mut signal = Vec::new();
    if let Some(r) = d.rssi {
        signal.push(format!("{r} dBm"));
    }
    if let Some(t) = d.tx_power {
        signal.push(format!("tx {t} dBm"));
    }
    if let Some(b) = d.battery {
        signal.push(format!("battery {b}%"));
    }
    if !signal.is_empty() {
        lines.push(row("signal", &signal.join(" · ")));
    }

    let mut flags = Vec::new();
    flags.push(if d.paired {
        if d.bonded { "paired" } else { "paired (not bonded)" }
    } else {
        "not paired"
    });
    flags.push(if d.trusted { "trusted" } else { "untrusted" });
    if d.blocked {
        flags.push("blocked");
    }
    if d.legacy_pairing {
        flags.push("legacy pairing");
    }
    if d.wake_allowed {
        flags.push("can wake host");
    }
    if d.connected && !d.services_resolved {
        flags.push("resolving services…");
    }
    if d.media_connected {
        flags.push("media control");
    }
    lines.push(row("flags", &flags.join(" · ")));

    let profiles = d.profile_names();
    if !profiles.is_empty() {
        lines.push(row("profiles", &profiles.join(", ")));
    } else if !d.uuids.is_empty() {
        let shown: Vec<String> = d.uuids.iter().take(3).map(|u| describe_uuid(u)).collect();
        lines.push(row("services", &shown.join(", ")));
    }

    let makers: Vec<String> = d
        .manufacturer_ids
        .iter()
        .map(|id| {
            company_name(*id)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{id:#06x}"))
        })
        .collect();
    if !makers.is_empty() {
        lines.push(row("adverts", &makers.join(", ")));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        action_hint(d),
        Style::default().fg(FG_DIM),
    )));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn action_hint(d: &Device) -> String {
    if d.blocked {
        "blocked — b unblocks it, f forgets it".into()
    } else if d.connected {
        "connected — d disconnects, t toggles trust, f forgets it".into()
    } else if d.paired {
        "↵ connects · f forgets the pairing · t toggles automatic reconnection".into()
    } else if d.name.is_empty() {
        "↵ pairs with this unnamed device — most such devices are beacons or phones that will refuse".into()
    } else {
        "↵ pairs and connects — put the device in pairing mode first".into()
    }
}

// ----------------------------------------------------------------------- modals

fn draw_prompt(frame: &mut Frame, p: &Prompt) {
    let height = 5 + p.fields.len() as u16 * 2;
    let area = centered(frame.area(), 62, height);
    frame.render_widget(Clear, area);

    let mut lines = vec![Line::from(Span::styled(
        p.hint.clone(),
        Style::default().fg(FG_DIM),
    ))];
    for (i, f) in p.fields.iter().enumerate() {
        lines.push(Line::from(""));
        lines.push(field_line(f, i == p.focus, p.reveal));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("↵ {}   ^u clear   esc cancel", p.submit_label()),
        Style::default().fg(FG_DIM),
    )));

    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(dialog(&p.title, ACCENT)),
        area,
    );
}

fn field_line(f: &Field, focused: bool, reveal: bool) -> Line<'static> {
    let shown = if f.secret && !reveal {
        "•".repeat(f.value.chars().count())
    } else {
        f.value.clone()
    };
    Line::from(vec![
        Span::styled(
            format!("{:<10}", f.label),
            Style::default().fg(if focused { ACCENT } else { FG_KEY }),
        ),
        Span::raw(shown),
        Span::styled(
            if focused { "▏" } else { "" },
            Style::default().fg(ACCENT).add_modifier(Modifier::RAPID_BLINK),
        ),
    ])
}

fn draw_confirm(frame: &mut Frame, c: &ConfirmKind) {
    let (title, body, keys, color) = match c {
        ConfirmKind::Forget { name, .. } => (
            "Forget device".to_string(),
            format!(
                "Remove “{name}” and its pairing keys?\nIt will have to be paired again before it can connect."
            ),
            "y confirm    esc cancel",
            Color::Yellow,
        ),
        ConfirmKind::Agent { title, body, .. } => (
            title.clone(),
            body.clone(),
            "y accept    n reject",
            Color::Magenta,
        ),
    };
    let area = centered(frame.area(), 58, 8);
    frame.render_widget(Clear, area);
    let mut lines = vec![Line::from("")];
    lines.extend(body.lines().map(|l| Line::from(l.to_string())));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(keys, Style::default().fg(FG_DIM))),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(dialog(&title, color)),
        area,
    );
}

fn draw_info(frame: &mut Frame, title: &str, text: &str) {
    let area = centered(frame.area(), 58, 7);
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from(""),
        Line::from(text.to_string()),
        Line::from(""),
        Line::from(Span::styled("any key hides this", Style::default().fg(FG_DIM))),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(dialog(title, Color::Magenta)),
        area,
    );
}

const HELP: &[(&str, &str)] = &[
    ("↑ ↓ / j k", "move within the focused pane"),
    ("tab / ← →", "switch between adapters and devices"),
    ("g / G", "jump to the first or last row"),
    ("", ""),
    ("enter", "pair (if needed) and connect to the selected device"),
    ("d", "disconnect the selected device"),
    ("f", "forget the device: remove it and its pairing keys"),
    ("t", "trust / untrust: let the device reconnect on its own"),
    ("b", "block / unblock all connections from the device"),
    ("n", "rename the selected device or adapter"),
    ("", ""),
    ("s / r", "start or stop scanning for nearby devices"),
    ("w", "turn the Bluetooth radio on or off"),
    ("v", "make this computer visible to other devices"),
    ("p", "allow or refuse incoming pairing requests"),
    ("", ""),
    ("esc", "dismiss a message or close a dialog"),
    ("q / ctrl-c", "quit"),
];

fn draw_help(frame: &mut Frame) {
    let area = centered(frame.area(), 70, HELP.len() as u16 + 4);
    frame.render_widget(Clear, area);
    let mut lines = vec![Line::from("")];
    for (k, v) in HELP {
        if k.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{k:<12}"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(*v),
        ]));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(dialog("Keys — any key closes", ACCENT)),
        area,
    );
}

// ---------------------------------------------------------------------- pieces

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn row(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<11}"), Style::default().fg(FG_KEY)),
        Span::raw(value.to_string()),
    ])
}

fn kv(key: &str, value: &str) -> Span<'static> {
    Span::styled(format!("{key} {value}"), Style::default().fg(FG_DIM))
}

fn signal_bars(rssi: i16) -> Vec<Span<'static>> {
    let filled = rssi_bars(rssi) as usize;
    let glyphs = ['▂', '▄', '▆', '█'];
    let color = rssi_color(rssi);
    glyphs
        .iter()
        .enumerate()
        .map(|(i, g)| {
            Span::styled(
                g.to_string(),
                if i < filled {
                    Style::default().fg(color)
                } else {
                    Style::default().fg(FG_DIM)
                },
            )
        })
        .collect()
}

fn rssi_color(rssi: i16) -> Color {
    match rssi_bars(rssi) {
        1 => Color::Red,
        2 => Color::Yellow,
        3 => Color::LightGreen,
        _ => Color::Green,
    }
}

fn battery_color(pct: u8) -> Color {
    match pct {
        0..=15 => Color::Red,
        16..=35 => Color::Yellow,
        _ => Color::Green,
    }
}

fn adapter_color(a: &Adapter) -> Color {
    match a.power_state {
        PowerState::OffBlocked => Color::Red,
        s if s.is_busy() => Color::Yellow,
        _ if a.powered => Color::Green,
        _ => Color::DarkGray,
    }
}

/// The cursor row. In the unfocused pane it stays visible — it is still where
/// the cursor will be on the next Tab — but faint enough not to compete.
fn selection_style(focused: bool) -> Style {
    if focused {
        Style::default().bg(Color::Rgb(38, 48, 60)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::Rgb(24, 26, 30))
    }
}

/// Left gutter marker on the cursor row, drawn only in the focused pane. The
/// blank keeps both panes on the same column grid so nothing shifts on Tab.
fn selection_marker(focused: bool) -> &'static str {
    if focused { "\u{258c}" } else { " " }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn duration(secs: u32) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// `Adapter1.Version` is the HCI version byte from the controller.
fn core_version(v: u8) -> Option<&'static str> {
    Some(match v {
        0 => "1.0b",
        1 => "1.1",
        2 => "1.2",
        3 => "2.0",
        4 => "2.1",
        5 => "3.0",
        6 => "4.0",
        7 => "4.1",
        8 => "4.2",
        9 => "5.0",
        10 => "5.1",
        11 => "5.2",
        12 => "5.3",
        13 => "5.4",
        14 => "6.0",
        15 => "6.1",
        _ => return None,
    })
}
