//! Drawing. Every frame is drawn from [`App`] alone.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap,
};

use super::app::{App, Focus, Item, LogView, Mode, PALETTE};
use super::fmt::{age, bar, bytes, clock, spark, trunc};
use super::model::{Replica, Rollout, Sandbox, Service, Stack};
use super::theme::Theme;
use crate::stack::now_secs;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if let Mode::Logs(v) = &app.mode {
        let [body, keys] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        logs(f, app, v, body);
        keybar(f, app, keys);
        return;
    }
    let events_h = if area.height >= 30 {
        7
    } else if area.height >= 20 {
        5
    } else {
        0
    };
    let [head, body, events_area, keys] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(events_h),
        Constraint::Length(1),
    ])
    .areas(area);
    header(f, app, head);
    let side_w = (area.width / 4).clamp(24, 36);
    let [side, detail_area] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Fill(1)]).areas(body);
    sidebar(f, app, side);
    detail(f, app, detail_area);
    if events_h > 0 {
        events(f, app, events_area);
    }
    keybar(f, app, keys);

    match &app.mode {
        Mode::Help => help(f, app, area),
        Mode::Confirm(c) => confirm(f, app, c, area),
        Mode::Prompt(p) => prompt(f, app, p, area),
        Mode::Palette(t) => palette(f, app, t, area),
        _ => {}
    }
}

fn pane<'a>(app: &App, title: &'a str, focused: bool) -> Block<'a> {
    let t = &app.theme;
    let b = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            if focused {
                t.accent().add_modifier(Modifier::BOLD)
            } else {
                t.heading()
            },
        ));
    if focused {
        b.border_type(BorderType::Rounded).border_style(t.accent())
    } else {
        b.border_style(t.faint())
    }
}

/// `glyph word`, both in the state's colour.
fn status_word<'a>(t: &Theme, state: &str) -> Vec<Span<'a>> {
    let (g, s) = t.status(state);
    vec![
        Span::styled(g, s),
        Span::raw(" "),
        Span::styled(state.to_lowercase(), s),
    ]
}

// ---- header -----------------------------------------------------------

fn header(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let h = &app.ov.host;
    let host = if h.hostname.is_empty() {
        "…"
    } else {
        &h.hostname
    };
    let mode = if app.daemon {
        vec![
            Span::styled("serve ", t.dim()),
            Span::styled("●", Style::default().fg(t.ok)),
        ]
    } else {
        vec![
            Span::styled("direct · read-only ", t.dim()),
            Span::styled("◌", Style::default().fg(t.warn)),
        ]
    };
    let mut right = vec![Span::styled(format!("isb {} · ", app.ov.isb), t.faint())];
    right.extend(mode);
    let left = Line::from(vec![
        Span::styled(" isb", t.accent().add_modifier(Modifier::BOLD)),
        Span::styled(" · ", t.faint()),
        Span::styled(host.to_string(), t.bold()),
    ]);
    let [l1, l2, rule] = Layout::vertical([Constraint::Length(1); 3]).areas(area);
    f.render_widget(Paragraph::new(left), l1);
    f.render_widget(
        Paragraph::new(Line::from(right).alignment(Alignment::Right)),
        Rect {
            width: l1.width.saturating_sub(1),
            ..l1
        },
    );

    let (hl, tot) = app.ov.stacks.iter().fold((0, 0), |(a, b), s| {
        let (h, t) = s.replicas();
        (a + h, b + t)
    });
    if app.updated.is_none() {
        f.render_widget(Paragraph::new(Span::styled(" connecting…", t.dim())), l2);
        f.render_widget(
            Paragraph::new(Span::styled("─".repeat(rule.width as usize), t.faint())),
            rule,
        );
        return;
    }
    let up = app.ov.sandboxes.iter().filter(|s| s.running()).count();
    let rep_style = if hl == tot {
        Style::default().fg(t.ok)
    } else {
        Style::default().fg(t.warn)
    };
    let mem_frac = if h.mem_total > 0 {
        h.mem_used as f64 / h.mem_total as f64
    } else {
        0.0
    };
    let mut stats = vec![Span::raw(" ")];
    if app.daemon {
        stats.extend([
            Span::styled("stacks ", t.dim()),
            Span::styled(app.ov.stacks.len().to_string(), t.bold()),
            Span::styled("   replicas ", t.dim()),
            Span::styled(
                format!("{hl}/{tot}"),
                rep_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled("   ", t.dim()),
        ]);
    }
    stats.extend([
        Span::styled("sandboxes ", t.dim()),
        Span::styled(app.ov.sandboxes.len().to_string(), t.bold()),
        Span::styled(format!(" ({up} up)"), t.dim()),
    ]);
    if area.width >= 100 {
        stats.extend([
            Span::styled("   cpu ", t.dim()),
            Span::styled(spark(&h.cpu_history, 12, Some(100.0)), t.accent()),
            Span::styled(format!(" {:>3.0}%", h.cpu_pct.unwrap_or(0.0)), t.bold()),
            Span::styled("   mem ", t.dim()),
            Span::styled(bar(mem_frac, 10), t.accent()),
            Span::styled(
                format!(" {}/{}", bytes(h.mem_used), bytes(h.mem_total)),
                t.bold(),
            ),
        ]);
        // An older daemon sends no storage numbers: leave it out.
        if h.disk_total > 0 && area.width >= 130 {
            stats.extend([
                Span::styled("   disk ", t.dim()),
                Span::styled(
                    bar(h.disk_used as f64 / h.disk_total as f64, 10),
                    t.accent(),
                ),
                Span::styled(
                    format!(" {}/{}", bytes(h.disk_used), bytes(h.disk_total)),
                    t.bold(),
                ),
            ]);
        }
    }
    if area.width >= 120 {
        stats.push(Span::styled(format!("   load {:.1}", h.load1), t.dim()));
    }
    f.render_widget(Paragraph::new(Line::from(stats)), l2);
    let status = match (&app.error, app.updated) {
        (Some(e), _) => Span::styled(format!("✖ {} ", trunc(e, 60)), Style::default().fg(t.err)),
        (None, None) => Span::raw(""),
        (None, Some(_)) => Span::raw(""),
    };
    f.render_widget(
        Paragraph::new(Line::from(status).alignment(Alignment::Right)),
        l2,
    );
    f.render_widget(
        Paragraph::new(Span::styled("─".repeat(rule.width as usize), t.faint())),
        rule,
    );
}

// ---- sidebar ----------------------------------------------------------

fn sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let focused = app.focus == Focus::Sidebar;
    let title = if app.filter.is_empty() {
        "overview".to_string()
    } else {
        format!("/{}", app.filter)
    };
    let block = pane(app, &title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let items = app.items();
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0;
    let mut section = None;
    for (n, it) in items.iter().enumerate() {
        let sec = matches!(it, Item::Stack(_));
        if section != Some(sec) {
            if section.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(
                if sec { "STACKS" } else { "SANDBOXES" },
                t.heading(),
            ));
            section = Some(sec);
        }
        if n == app.sel {
            sel_line = lines.len();
        }
        let (glyph, gstyle, name, right, rstyle) = match it {
            Item::Stack(i) => {
                let s = &app.ov.stacks[*i];
                let (g, st) = t.status(s.state());
                let (h, tot) = s.replicas();
                let r = if s.state() == "updating" {
                    format!("↻ {h}/{tot}")
                } else {
                    format!("{h}/{tot}")
                };
                let rs = if h == tot {
                    t.dim()
                } else {
                    Style::default().fg(t.warn)
                };
                (g, st, s.name.clone(), r, rs)
            }
            Item::Sandbox(i) => {
                let s = &app.ov.sandboxes[*i];
                let (g, st) = t.status(&s.status);
                let r = match s.labels.get("isb.owner") {
                    Some(_) => "mcp".to_string(),
                    None if s.kind == "oci" => "oci".to_string(),
                    None if s.kind == "virtual-machine" => "vm".to_string(),
                    None => String::new(),
                };
                (g, st, s.name.clone(), r, t.faint())
            }
        };
        let name_w = w.saturating_sub(right.chars().count() + 3);
        let name = trunc(&name, name_w);
        let pad = w.saturating_sub(2 + name.chars().count() + right.chars().count());
        let selected = n == app.sel;
        let mut line = Line::from(vec![
            Span::styled(glyph, gstyle),
            Span::raw(" "),
            Span::styled(name, if selected { t.bold() } else { t.fg() }),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, rstyle),
        ]);
        if selected {
            line = line.style(if focused {
                t.selected()
            } else {
                t.selected().add_modifier(Modifier::DIM)
            });
        }
        lines.push(line);
    }
    if items.is_empty() {
        lines.push(Line::styled(
            if app.filter.is_empty() {
                "nothing yet"
            } else {
                "no match"
            },
            t.dim(),
        ));
    }
    let h = inner.height as usize;
    let scroll = sel_line.saturating_sub(h.saturating_sub(2));
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
}

// ---- detail -----------------------------------------------------------

fn detail(f: &mut Frame, app: &App, area: Rect) {
    match app.selected() {
        Some(Item::Stack(i)) => stack(f, app, &app.ov.stacks[i], area),
        Some(Item::Sandbox(i)) => sandbox(f, app, &app.ov.sandboxes[i], area),
        None => empty(f, app, area),
    }
}

fn empty(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = pane(app, "welcome", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![
        Line::raw(""),
        Line::styled("Nothing is running here yet.", t.bold()),
        Line::raw(""),
    ];
    if app.daemon {
        lines.push(Line::from(vec![
            Span::styled("Deploy a stack:  ", t.dim()),
            Span::styled("d", t.accent()),
            Span::styled(" or ", t.dim()),
            Span::styled(":deploy ./isb.yaml", t.accent()),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("Stacks need the daemon:  ", t.dim()),
            Span::styled("isb serve install", t.accent()),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Run a sandbox:   ", t.dim()),
        Span::styled("isb create NAME -i dev-base", t.accent()),
    ]));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

fn stack(f: &mut Frame, app: &App, s: &Stack, area: Rect) {
    let t = &app.theme;
    let focused = app.focus != Focus::Sidebar;
    let block = pane(app, &s.name, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let svc = s.services.get(app.svc);
    let rollout_h = svc
        .and_then(|v| v.rollout.as_ref())
        .map(|r| r.slots.len() as u16 + 4)
        .unwrap_or(0);
    let services_h = s.services.len() as u16 + 2;
    let [title, _, services, roll, replicas] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(services_h),
        Constraint::Length(rollout_h),
        Constraint::Fill(1),
    ])
    .areas(inner);

    let mut tl = status_word(t, s.state());
    if s.deployed_at > 0 {
        let by = if s.deployed_by.is_empty() {
            String::new()
        } else {
            format!(" by {}", s.deployed_by)
        };
        tl.push(Span::styled(
            format!("   deployed {} ago{by}", age(s.deployed_at, now_secs())),
            t.dim(),
        ));
    }
    if s.has_previous {
        tl.push(Span::styled("   ↶ rollback available", t.faint()));
    }
    f.render_widget(Paragraph::new(Line::from(tl)), title);
    service_table(f, app, s, services);
    if let (Some(v), true) = (svc, rollout_h > 0) {
        if let Some(r) = &v.rollout {
            rollout(f, app, v, r, roll);
        }
    }
    if let Some(v) = svc {
        replica_table(f, app, v, replicas);
    }
}

fn service_table(f: &mut Frame, app: &App, s: &Stack, area: Rect) {
    let t = &app.theme;
    // Columns give way right to left as the pane narrows.
    let wide = area.width >= 100;
    let show_traffic = area.width >= 72;
    let show_rev = area.width >= 82;
    let rows: Vec<Row> = s
        .services
        .iter()
        .map(|v| {
            let (g, gs) = t.status(&v.state);
            let dots: String = (0..v.replicas.min(8))
                .map(|i| if i < v.healthy { '●' } else { '○' })
                .collect();
            let port = v
                .ports
                .first()
                .map(|p| {
                    let more = if v.ports.len() > 1 {
                        format!(" +{}", v.ports.len() - 1)
                    } else {
                        String::new()
                    };
                    format!(
                        ":{} → {}{more}",
                        p.listen.rsplit(':').next().unwrap_or(""),
                        p.target
                    )
                })
                .unwrap_or_else(|| "-".into());
            let rate: Vec<f32> = v
                .ports
                .first()
                .map(|p| p.rate_history.clone())
                .unwrap_or_default();
            let last = rate.last().copied().unwrap_or(0.0);
            let mut cells = vec![
                Cell::from(Line::from(vec![
                    Span::styled(g, gs),
                    Span::raw(" "),
                    Span::styled(v.service.clone(), t.bold()),
                ])),
                Cell::from(Span::styled(v.state.clone(), gs)),
                Cell::from(Line::from(vec![
                    Span::styled(dots, gs),
                    Span::styled(format!(" {}/{}", v.healthy, v.replicas), t.fg()),
                ])),
                Cell::from(Span::styled(port, t.fg())),
            ];
            if show_traffic {
                cells.push(Cell::from(if v.ports.is_empty() {
                    Line::styled("-", t.faint())
                } else {
                    Line::from(vec![
                        Span::styled(spark(&rate, 8, Some(peak(&rate, 5.0))), t.accent()),
                        Span::styled(format!(" {last:>4.0}/s"), t.dim()),
                    ])
                }));
            }
            if show_rev {
                cells.push(Cell::from(Span::styled(v.rev.clone(), t.faint())));
            }
            if wide {
                cells.push(Cell::from(Span::styled(trunc(&v.image, 28), t.dim())));
            }
            Row::new(cells)
        })
        .collect();
    let mut widths = vec![
        Constraint::Fill(1),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(17),
    ];
    let mut head = vec!["SERVICE", "STATE", "REPLICAS", "PORT"];
    if show_traffic {
        widths.push(Constraint::Length(15));
        head.push("TRAFFIC");
    }
    if show_rev {
        widths.push(Constraint::Length(9));
        head.push("REV");
    }
    if wide {
        widths[0] = Constraint::Length(16);
        widths.push(Constraint::Fill(1));
        head.push("IMAGE");
    }
    let focused = app.focus == Focus::Services;
    let table = Table::new(rows, widths)
        .header(Row::new(head).style(t.heading()).bottom_margin(0))
        .row_highlight_style(if focused {
            t.selected()
        } else {
            t.selected().add_modifier(Modifier::DIM)
        })
        .column_spacing(1);
    let mut st =
        TableState::default().with_selected((app.focus != Focus::Sidebar).then_some(app.svc));
    f.render_stateful_widget(table, area, &mut st);
}

fn rollout(f: &mut Frame, app: &App, v: &Service, r: &Rollout, area: Rect) {
    let t = &app.theme;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(t.warn))
        .title(Span::styled(
            format!(" {} rolling out ", v.service),
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let frac = if r.total > 0 {
        r.done as f64 / r.total as f64
    } else {
        0.0
    };
    let from = r
        .slots
        .iter()
        .find_map(|s| s.old_rev.clone())
        .unwrap_or_else(|| "new".into());
    let roomy = inner.width >= 100;
    let detail = if roomy {
        format!(
            "   {} · {} at a time · {} ago   ",
            r.order,
            r.parallelism,
            age(r.started_at, now_secs())
        )
    } else {
        format!("   {}   ", r.order)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{from} → "), t.dim()),
        Span::styled(r.to_rev.clone(), t.bold()),
        Span::styled(detail, t.dim()),
        Span::styled(
            bar(frac, if roomy { 18 } else { 12 }),
            Style::default().fg(t.warn),
        ),
        Span::styled(format!(" {}/{}", r.done, r.total), t.bold()),
    ])];
    lines.push(Line::raw(""));
    // Instance names only when there is room for both sides.
    let names = inner.width >= 100;
    for s in &r.slots {
        let old = match &s.old {
            Some(o) => {
                let (g, gs) = t.status(&s.old_state);
                let mut v = vec![
                    Span::styled(
                        format!("{:<10}", trunc(s.old_rev.as_deref().unwrap_or(""), 10)),
                        t.faint(),
                    ),
                    Span::styled(g, gs),
                    Span::styled(format!(" {:<10}", s.old_state), gs),
                ];
                if names {
                    v.push(Span::styled(format!("{:<20}", trunc(o, 20)), t.dim()));
                }
                v
            }
            None => vec![Span::styled(
                format!("{:<w$}", "(new slot)", w = if names { 42 } else { 22 }),
                t.faint(),
            )],
        };
        let arrow = if s.new_state == "waiting" {
            "    ·    "
        } else {
            "  ━━━▶  "
        };
        let (g, gs) = t.status(&s.new_state);
        let mut line = vec![Span::styled(format!("slot {:<3}", s.slot), t.dim())];
        line.extend(old);
        line.push(Span::styled(
            arrow,
            if s.new_state == "waiting" {
                t.faint()
            } else {
                t.accent()
            },
        ));
        line.extend([
            Span::styled(format!("{:<10}", r.to_rev), t.faint()),
            Span::styled(g, gs),
            Span::styled(format!(" {:<11}", s.new_state), gs),
        ]);
        if names {
            line.push(Span::styled(s.new.clone().unwrap_or_default(), t.dim()));
        }
        lines.push(Line::from(line));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn replica_table(f: &mut Frame, app: &App, v: &Service, area: Rect) {
    let t = &app.theme;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(t.faint())
        .title(Line::from(vec![
            Span::styled(format!(" {} ", v.service), t.heading()),
            Span::styled("replicas ", t.faint()),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let probe_line = (app.focus == Focus::Replicas)
        .then(|| v.instances.get(app.rep))
        .flatten()
        .filter(|r| !r.last_probe.is_empty() && r.health != "healthy");
    let msg = v.message.as_ref();
    let foot_h = probe_line.is_some() as u16
        + msg.is_some() as u16
        + v.ports.iter().filter(|p| p.error.is_some()).count() as u16;
    let [table_area, foot] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(foot_h)]).areas(inner);

    let cols = ReplicaCols::fit(table_area.width);
    let rows: Vec<Row> = v
        .instances
        .iter()
        .map(|r| replica_row(t, r, &cols))
        .collect();
    let focused = app.focus == Focus::Replicas;
    let table = Table::new(rows, cols.widths())
        .header(Row::new(cols.header()).style(t.heading()))
        .row_highlight_style(t.selected())
        .column_spacing(1);
    let mut st = TableState::default().with_selected(focused.then_some(app.rep));
    if v.instances.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("no replicas", t.dim())),
            table_area,
        );
    } else {
        f.render_stateful_widget(table, table_area, &mut st);
    }

    let mut lines = Vec::new();
    if let Some(m) = msg {
        let style = if v.state == "failing" {
            Style::default().fg(t.err)
        } else {
            Style::default().fg(t.warn)
        };
        lines.push(Line::from(vec![
            Span::styled("! ", style),
            Span::styled(m.clone(), style),
        ]));
    }
    for p in &v.ports {
        if let Some(e) = &p.error {
            lines.push(Line::styled(
                format!("! port {}: {e}", p.listen),
                Style::default().fg(t.err),
            ));
        }
    }
    if let Some(r) = probe_line {
        let last = r.last_probe.lines().last().unwrap_or("").to_string();
        lines.push(Line::from(vec![
            Span::styled("probe ", t.dim()),
            Span::styled(last, Style::default().fg(t.warn)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), foot);
}

/// Which replica columns fit, decided once per frame from the pane width.
struct ReplicaCols {
    status: bool,
    spark: bool,
    mem: bool,
    disk: bool,
    ip: bool,
    lb: bool,
}

impl ReplicaCols {
    fn fit(w: u16) -> Self {
        ReplicaCols {
            status: w >= 60,
            spark: w >= 76,
            mem: w >= 66,
            disk: w >= 84,
            ip: w >= 92,
            lb: w >= 100,
        }
    }

    fn widths(&self) -> Vec<Constraint> {
        let mut v = vec![Constraint::Length(2), Constraint::Min(14)];
        if self.status {
            v.push(Constraint::Length(9));
        }
        v.push(Constraint::Length(11));
        if self.ip {
            v.push(Constraint::Length(15));
        }
        v.push(Constraint::Length(if self.spark { 16 } else { 5 }));
        if self.mem {
            v.push(Constraint::Length(6));
        }
        if self.disk {
            v.push(Constraint::Length(6));
        }
        if self.lb {
            v.extend([Constraint::Length(3), Constraint::Length(3)]);
        }
        v
    }

    fn header(&self) -> Vec<&'static str> {
        let mut v = vec!["#", "INSTANCE"];
        if self.status {
            v.push("STATUS");
        }
        v.push("HEALTH");
        if self.ip {
            v.push("IP");
        }
        v.push("CPU");
        if self.mem {
            v.push("MEM");
        }
        if self.disk {
            v.push("DISK");
        }
        if self.lb {
            v.extend(["LB", "↻"]);
        }
        v
    }
}

fn replica_row<'a>(t: &Theme, r: &Replica, c: &ReplicaCols) -> Row<'a> {
    let (sg, ss) = t.status(&r.status);
    let (hg, hs) = t.status(&r.health);
    let cpu = r
        .cpu_pct
        .map(|c| format!("{c:>4.0}%"))
        .unwrap_or_else(|| "    -".into());
    let mut cells = vec![
        Cell::from(Span::styled(r.slot.to_string(), t.dim())),
        Cell::from(Span::styled(r.name.clone(), t.fg())),
    ];
    if c.status {
        cells.push(Cell::from(Line::from(vec![
            Span::styled(sg, ss),
            Span::styled(format!(" {}", r.status.to_lowercase()), ss),
        ])));
    }
    cells.push(Cell::from(Line::from(vec![
        Span::styled(hg, hs),
        Span::styled(
            format!(
                " {}",
                if r.health == "none" {
                    "running"
                } else {
                    &r.health
                }
            ),
            hs,
        ),
    ])));
    if c.ip {
        cells.push(Cell::from(Span::styled(
            r.ip.clone().unwrap_or_else(|| "-".into()),
            t.dim(),
        )));
    }
    cells.push(Cell::from(if c.spark {
        Line::from(vec![
            Span::styled(
                spark(&r.cpu_history, 10, Some(peak(&r.cpu_history, 25.0))),
                t.accent(),
            ),
            Span::styled(cpu, t.fg()),
        ])
    } else {
        Line::styled(cpu, t.fg())
    }));
    if c.mem {
        cells.push(Cell::from(Span::styled(
            r.mem_bytes.map(bytes).unwrap_or_else(|| "-".into()),
            t.fg(),
        )));
    }
    if c.disk {
        cells.push(Cell::from(Span::styled(
            r.disk_bytes.map(bytes).unwrap_or_else(|| "-".into()),
            t.fg(),
        )));
    }
    if c.lb {
        cells.push(Cell::from(if r.in_rotation {
            Span::styled(" ⇄", Style::default().fg(t.ok))
        } else {
            Span::styled(" ·", t.faint())
        }));
        cells.push(Cell::from(Span::styled(
            if r.restarts > 0 {
                r.restarts.to_string()
            } else {
                String::new()
            },
            Style::default().fg(t.warn),
        )));
    }
    Row::new(cells)
}

fn sandbox(f: &mut Frame, app: &App, s: &Sandbox, area: Rect) {
    let t = &app.theme;
    let block = pane(app, &s.name, false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let kv = |k: &str, v: Vec<Span<'static>>| {
        let mut l = vec![Span::styled(format!("{k:<10}"), t.dim())];
        l.extend(v);
        Line::from(l)
    };
    let kind = match s.kind.as_str() {
        "virtual-machine" => "virtual machine",
        "oci" => "application container (OCI)",
        _ => "system container",
    };
    let mut lines = vec![
        Line::from(status_word(t, &s.status)),
        Line::raw(""),
        kv("kind", vec![Span::styled(kind.to_string(), t.fg())]),
        kv("image", vec![Span::styled(s.image.clone(), t.fg())]),
        kv(
            "address",
            vec![Span::styled(
                s.ip.clone().unwrap_or_else(|| "-".into()),
                t.fg(),
            )],
        ),
    ];
    if s.running() {
        lines.push(kv(
            "cpu",
            vec![
                Span::styled(
                    spark(&s.cpu_history, 24, Some(peak(&s.cpu_history, 25.0))),
                    t.accent(),
                ),
                Span::styled(
                    s.cpu_pct
                        .map(|c| format!(" {c:.0}%"))
                        .unwrap_or_else(|| " -".into()),
                    t.bold(),
                ),
            ],
        ));
        lines.push(kv(
            "memory",
            vec![Span::styled(
                s.mem_bytes.map(bytes).unwrap_or_else(|| "-".into()),
                t.bold(),
            )],
        ));
    }
    if let Some(d) = s.disk_bytes {
        lines.push(kv("disk", vec![Span::styled(bytes(d), t.bold())]));
    }
    lines.push(kv(
        "created",
        vec![Span::styled(
            s.created_at
                .chars()
                .take(19)
                .collect::<String>()
                .replace('T', " "),
            t.fg(),
        )],
    ));
    let labels: Vec<String> = s
        .labels
        .iter()
        .filter(|(k, _)| !k.starts_with("isb.") || k.as_str() == "isb.owner")
        .map(|(k, v)| {
            if v.is_empty() {
                k.clone()
            } else {
                format!("{k}={v}")
            }
        })
        .collect();
    if !labels.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("LABELS", t.heading()));
        for l in labels {
            lines.push(Line::styled(format!("  {l}"), t.fg()));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ---- events, keys -----------------------------------------------------

fn events(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(t.faint())
        .title(Span::styled(" events ", t.heading()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !app.daemon {
        f.render_widget(
            Paragraph::new(Span::styled(
                " the event feed comes from isb serve, which is not running",
                t.dim(),
            )),
            inner,
        );
        return;
    }
    let lines: Vec<Line> = app
        .events
        .iter()
        .rev()
        .take(inner.height as usize)
        .map(|e| {
            let scope = if e.service.is_empty() {
                e.stack.clone()
            } else {
                format!("{}/{}", e.stack, e.service)
            };
            let (g, gs) = match e.level.as_str() {
                "error" => ("✖", Style::default().fg(t.err)),
                "warn" => ("▲", Style::default().fg(t.warn)),
                _ => ("·", t.faint()),
            };
            Line::from(vec![
                Span::styled(format!(" {} ", clock(e.at)), t.faint()),
                Span::styled(g, gs),
                Span::styled(format!(" {:<18} ", trunc(&scope, 18)), t.dim()),
                Span::styled(
                    e.message.clone(),
                    if e.level == "info" { t.fg() } else { gs },
                ),
            ])
        })
        .collect();
    let lines = if lines.is_empty() {
        vec![Line::styled(" nothing has happened yet", t.faint())]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(lines), inner);
}

fn keybar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    match &app.mode {
        Mode::Filter(text) => {
            let l = Line::from(vec![
                Span::styled(" /", t.accent().add_modifier(Modifier::BOLD)),
                Span::styled(text.clone(), t.bold()),
                Span::styled("▏", t.accent()),
            ]);
            f.render_widget(Paragraph::new(l), area);
            return;
        }
        Mode::Palette(_) => return,
        _ => {}
    }
    let mut hints = app.hints();
    let width = |h: &[(&str, &str)]| -> usize {
        h.iter()
            .map(|(k, d)| k.chars().count() + d.chars().count() + 3)
            .sum::<usize>()
            + 1
    };
    while hints.len() > 2 && width(&hints) > area.width as usize {
        let last = hints.pop().unwrap();
        hints.pop();
        hints.push(last);
    }
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in hints {
        spans.push(Span::styled(k, t.accent().add_modifier(Modifier::BOLD)));
        if !d.is_empty() {
            spans.push(Span::styled(format!(" {d}"), t.dim()));
        }
        spans.push(Span::styled("  ", t.faint()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let right = if let Some(b) = &app.busy {
        Some(Span::styled(
            format!("◐ {b}… "),
            Style::default().fg(t.warn),
        ))
    } else if let Some((lvl, msg, at)) = &app.toast {
        (at.elapsed().as_secs() < 6).then(|| {
            let style = match lvl.as_str() {
                "error" => Style::default().fg(t.err),
                "warn" => Style::default().fg(t.warn),
                _ => Style::default().fg(t.ok),
            };
            Span::styled(
                format!("{} ", trunc(msg, 70)),
                style.add_modifier(Modifier::BOLD),
            )
        })
    } else {
        None
    };
    if let Some(r) = right {
        let w = r.width() as u16;
        let x = area.x + area.width.saturating_sub(w);
        let a = Rect {
            x,
            width: area.width.min(w),
            ..area
        };
        f.render_widget(Clear, a);
        f.render_widget(Paragraph::new(r), a);
    }
}

// ---- overlays ---------------------------------------------------------

/// A sparkline's top: its largest value, but never below `floor`, so an
/// idle series draws low rather than stretched to full height.
fn peak(v: &[f32], floor: f32) -> f32 {
    v.iter().copied().fold(floor, f32::max)
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn modal<'a>(app: &App, title: String, danger: bool) -> Block<'a> {
    let t = &app.theme;
    let c = if danger { t.err } else { t.accent };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(c))
        .padding(Padding::new(2, 2, 1, 0))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(c).add_modifier(Modifier::BOLD),
        ))
}

fn help(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let sections: [(&str, &[(&str, &str)]); 3] = [
        (
            "move",
            &[
                ("↑↓ j k", "select"),
                ("⏎ tab →", "into services, then replicas"),
                ("esc ← ⇧tab", "back out"),
                ("g G", "top, bottom"),
                ("/", "filter"),
                (":", "command palette"),
            ],
        ),
        (
            "stacks",
            &[
                ("l", "logs of the service (replica, when one is selected)"),
                ("e", "shell in a replica"),
                ("s", "scale"),
                ("r", "redeploy (fresh replicas, rolling)"),
                ("b", "roll back to the previous deployment"),
                ("x", "remove the stack"),
                ("d", "deploy a compose file"),
            ],
        ),
        (
            "sandboxes",
            &[
                ("l", "logs"),
                ("e", "shell"),
                ("t", "start or stop"),
                ("x", "remove"),
            ],
        ),
    ];
    let mut lines = Vec::new();
    for (name, keys) in sections {
        lines.push(Line::styled(name.to_uppercase(), t.heading()));
        for (k, d) in keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<12}"), t.accent()),
                Span::styled(d.to_string(), t.fg()),
            ]));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        "q quits · R refreshes now · NO_COLOR=1 for monochrome",
        t.dim(),
    ));
    let a = centered(area, 76, lines.len() as u16 + 3);
    f.render_widget(Clear, a);
    f.render_widget(
        Paragraph::new(lines).block(modal(app, "keys".into(), false)),
        a,
    );
}

fn confirm(f: &mut Frame, app: &App, c: &super::app::Confirm, area: Rect) {
    let t = &app.theme;
    let h = c.lines.len() as u16 + if c.require.is_some() { 8 } else { 6 };
    let a = centered(area, 70, h);
    f.render_widget(Clear, a);
    let mut lines: Vec<Line> = c
        .lines
        .iter()
        .map(|(k, v)| {
            let style = match v.split_whitespace().next() {
                Some("remove") => Style::default().fg(t.err),
                Some("update") => Style::default().fg(t.warn),
                Some("create") => Style::default().fg(t.ok),
                _ => t.fg(),
            };
            Line::from(vec![
                Span::styled(format!("{k:<12}"), t.dim()),
                Span::styled(v.clone(), style),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    match &c.require {
        Some(word) => {
            lines.push(Line::from(vec![
                Span::styled("type ", t.dim()),
                Span::styled(word.clone(), t.bold()),
                Span::styled(" to confirm", t.dim()),
            ]));
            let ok = c.typed == *word;
            lines.push(Line::from(vec![
                Span::styled("› ", t.accent()),
                Span::styled(
                    c.typed.clone(),
                    if ok {
                        Style::default().fg(t.ok)
                    } else {
                        t.bold()
                    },
                ),
                Span::styled("▏", t.accent()),
            ]));
        }
        None => lines.push(Line::from(vec![
            Span::styled("y", t.accent().add_modifier(Modifier::BOLD)),
            Span::styled(" yes   ", t.dim()),
            Span::styled("n", t.accent().add_modifier(Modifier::BOLD)),
            Span::styled(" no", t.dim()),
        ])),
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(modal(app, c.title.clone(), c.danger)),
        a,
    );
}

fn prompt(f: &mut Frame, app: &App, p: &super::app::Prompt, area: Rect) {
    let t = &app.theme;
    let a = centered(area, 46, 7);
    f.render_widget(Clear, a);
    let lines = vec![
        Line::styled(p.label.clone(), t.dim()),
        Line::from(vec![
            Span::styled("› ", t.accent()),
            Span::styled(p.text.clone(), t.bold()),
            Span::styled("▏", t.accent()),
            Span::styled("   ↑↓ adjust", t.faint()),
        ]),
    ];
    f.render_widget(
        Paragraph::new(lines).block(modal(app, p.title.clone(), false)),
        a,
    );
}

fn palette(f: &mut Frame, app: &App, text: &str, area: Rect) {
    let t = &app.theme;
    let first = text.split_whitespace().next().unwrap_or("");
    let matches: Vec<&(&str, &str)> = PALETTE
        .iter()
        .filter(|(c, _)| {
            first.is_empty()
                || c.starts_with(first)
                || first.starts_with(c.split_whitespace().next().unwrap_or(""))
        })
        .collect();
    let h = matches.len() as u16 + 4;
    let a = Rect {
        x: area.x + 2,
        y: area.y + area.height.saturating_sub(h + 1),
        width: area.width.saturating_sub(4).min(72),
        height: h,
    };
    f.render_widget(Clear, a);
    let mut lines = vec![Line::from(vec![
        Span::styled(":", t.accent().add_modifier(Modifier::BOLD)),
        Span::styled(text.to_string(), t.bold()),
        Span::styled("▏", t.accent()),
    ])];
    lines.push(Line::raw(""));
    for (c, d) in matches {
        lines.push(Line::from(vec![
            Span::styled(format!("{c:<24}"), t.accent()),
            Span::styled(d.to_string(), t.dim()),
        ]));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.accent())
        .padding(Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), a);
}

fn logs(f: &mut Frame, app: &App, v: &LogView, area: Rect) {
    let t = &app.theme;
    let mut title = format!("logs · {}", v.target.title());
    if let Some(n) = v.only {
        title.push_str(&format!(" · replica {n}"));
    }
    let state = if v.loading {
        "loading"
    } else if v.follow {
        "following"
    } else {
        "paused"
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.accent())
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            t.accent().add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(vec![
                Span::styled(
                    if v.follow { " ● " } else { " ◫ " },
                    if v.follow {
                        Style::default().fg(t.ok)
                    } else {
                        Style::default().fg(t.warn)
                    },
                ),
                Span::styled(format!("{state} "), t.dim()),
            ])
            .alignment(Alignment::Right),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Replica colours cycle through the accent palette so interleaved lines
    // stay attributable at a glance.
    let colors = [t.accent, t.info, t.ok, t.warn, t.dim];
    let shown: Vec<&super::app::LogLine> = v
        .lines
        .iter()
        .filter(|l| v.only.is_none_or(|o| l.slot == o))
        .collect();
    let mut lines: Vec<Line> = shown
        .iter()
        .map(|l| {
            let c = colors[(l.slot as usize).saturating_sub(1) % colors.len()];
            let mut spans = Vec::new();
            if l.slot > 0 {
                spans.push(Span::styled(
                    format!("{} ", l.slot),
                    Style::default().fg(c).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled("│ ", t.faint()));
            }
            if !l.time.is_empty() {
                spans.push(Span::styled(
                    format!("{} ", l.time.get(11..19).unwrap_or(&l.time)),
                    t.faint(),
                ));
            }
            let lower = l.text.to_lowercase();
            let style = if lower.contains("error")
                || lower.contains("traceback")
                || lower.contains("panic")
            {
                Style::default().fg(t.err)
            } else if lower.contains("warn") {
                Style::default().fg(t.warn)
            } else {
                t.fg()
            };
            spans.push(Span::styled(l.text.clone(), style));
            Line::from(spans)
        })
        .collect();
    if let Some(e) = &v.error {
        lines.push(Line::styled(format!("✖ {e}"), Style::default().fg(t.err)));
    }
    if lines.is_empty() && !v.loading {
        lines.push(Line::styled("no output yet", t.dim()));
    }
    let h = inner.height as usize;
    let total = lines.len();
    let top = total.saturating_sub(h).saturating_sub(v.scroll);
    let mut p = Paragraph::new(lines).scroll((top as u16, 0));
    if v.wrap {
        p = p.wrap(Wrap { trim: false });
    }
    f.render_widget(p, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, Focus, LogLine, LogTarget, LogView};
    use crate::tui::model::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub fn fixture() -> Overview {
        let rep = |n: u32, health: &str, rot: bool| Replica {
            name: format!("e2e-web-{n}-a{n}b{n}"),
            slot: n,
            rev: "8a17158c".into(),
            status: "Running".into(),
            health: health.into(),
            ip: Some(format!("10.180.0.{}", 30 + n)),
            in_rotation: rot,
            restarts: if n == 3 { 1 } else { 0 },
            last_probe: if n == 3 {
                "Traceback...\nConnectionRefusedError: [Errno 111]".into()
            } else {
                String::new()
            },
            cpu_pct: Some(n as f32 * 3.0),
            cpu_history: (0..20).map(|i| ((i * n) % 7) as f32).collect(),
            mem_bytes: Some(80 * 1024 * 1024 + n as u64 * 3_000_000),
            disk_bytes: Some(410 * 1024 * 1024 + n as u64 * 1_000_000),
        };
        Overview {
            isb: "0.7.0".into(),
            host: Host {
                hostname: "titan".into(),
                cpus: 40,
                cpu_pct: Some(12.0),
                cpu_history: (0..30).map(|i| (i % 9) as f32 * 4.0).collect(),
                mem_used: 18 * 1024 * 1024 * 1024,
                mem_total: 251 * 1024 * 1024 * 1024,
                disk_used: 588 * 1024 * 1024 * 1024,
                disk_total: 902 * 1024 * 1024 * 1024,
                load1: 2.4,
            },
            stacks: vec![
                Stack {
                    name: "e2e".into(),
                    org: "default".into(),
                    deployed_at: now_secs() - 130,
                    deployed_by: "local(uid 1000)".into(),
                    has_previous: true,
                    converged: false,
                    services: vec![
                        Service {
                            service: "web".into(),
                            image: "dev-base".into(),
                            rev: "8a17158c".into(),
                            replicas: 3,
                            running: 3,
                            healthy: 2,
                            state: "updating".into(),
                            instances: vec![
                                rep(1, "healthy", true),
                                rep(2, "healthy", true),
                                rep(3, "starting", false),
                            ],
                            ports: vec![Port {
                                listen: "127.0.0.1:18080".into(),
                                target: 8000,
                                backends: vec!["10.180.0.31:8000".into()],
                                rate_history: (0..20).map(|i| (i % 5) as f32 * 10.0).collect(),
                                ..Default::default()
                            }],
                            rollout: Some(Rollout {
                                to_rev: "8a17158c".into(),
                                order: "start-first".into(),
                                parallelism: 1,
                                done: 1,
                                total: 3,
                                started_at: now_secs() - 40,
                                slots: vec![
                                    SlotRollout {
                                        slot: 1,
                                        old: Some("e2e-web-1-59ff".into()),
                                        old_rev: Some("f739806f".into()),
                                        old_state: "retired".into(),
                                        new: Some("e2e-web-1-a1b1".into()),
                                        new_state: "serving".into(),
                                    },
                                    SlotRollout {
                                        slot: 2,
                                        old: Some("e2e-web-2-445e".into()),
                                        old_rev: Some("f739806f".into()),
                                        old_state: "draining".into(),
                                        new: Some("e2e-web-2-a2b2".into()),
                                        new_state: "probing".into(),
                                    },
                                    SlotRollout {
                                        slot: 3,
                                        old: Some("e2e-web-3-3e68".into()),
                                        old_rev: Some("f739806f".into()),
                                        old_state: "serving".into(),
                                        new: None,
                                        new_state: "waiting".into(),
                                    },
                                ],
                            }),
                            ..Default::default()
                        },
                        Service {
                            service: "cache".into(),
                            image: "docker:redis:7-alpine".into(),
                            rev: "03848220".into(),
                            replicas: 1,
                            running: 1,
                            healthy: 1,
                            state: "converged".into(),
                            ..Default::default()
                        },
                    ],
                },
                Stack {
                    name: "monitoring".into(),
                    services: vec![Service {
                        service: "grafana".into(),
                        replicas: 1,
                        state: "failing".into(),
                        message: Some("slot 1: image \"grafana\" not found locally".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            sandboxes: vec![
                Sandbox {
                    name: "build-box".into(),
                    project: "default".into(),
                    status: "Running".into(),
                    kind: "container".into(),
                    image: "Ubuntu noble amd64".into(),
                    ip: Some("10.180.0.9".into()),
                    cpu_pct: Some(140.0),
                    cpu_history: (0..30).map(|i| (i % 6) as f32 * 30.0).collect(),
                    mem_bytes: Some(4 * 1024 * 1024 * 1024),
                    disk_bytes: Some(12 * 1024 * 1024 * 1024),
                    labels: [("owner".to_string(), "me".to_string())].into(),
                    created_at: "2026-10-03T01:00:00Z".into(),
                },
                Sandbox {
                    name: "agent-k7q2".into(),
                    status: "Running".into(),
                    kind: "container".into(),
                    labels: [("isb.owner".to_string(), "mcp:you@x".to_string())].into(),
                    ..Default::default()
                },
                Sandbox {
                    name: "old-dev".into(),
                    status: "Stopped".into(),
                    kind: "virtual-machine".into(),
                    ..Default::default()
                },
            ],
            events_seq: 3,
        }
    }

    pub fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn app() -> App {
        let mut a = App::new(Theme::mono(), true);
        a.set_overview(fixture());
        a.add_events(vec![
            Event {
                seq: 1,
                at: 1_700_000_000_000,
                level: "info".into(),
                stack: "e2e".into(),
                service: "web".into(),
                message: "rolling out rev 8a17158c to 3 slot(s), start-first".into(),
                ..Default::default()
            },
            Event {
                seq: 2,
                at: 1_700_000_005_000,
                level: "warn".into(),
                stack: "e2e".into(),
                service: "web".into(),
                message: "e2e-web-3-a3b3 is unhealthy (connection refused); restarting its app"
                    .into(),
                ..Default::default()
            },
            Event {
                seq: 3,
                at: 1_700_000_009_000,
                level: "error".into(),
                stack: "monitoring".into(),
                service: "grafana".into(),
                message: "slot 1: image not found".into(),
                ..Default::default()
            },
        ]);
        a
    }

    /// Dump a screen for eyeballing: `ISB_TUI_DUMP=dir cargo test tui`.
    fn dump(name: &str, s: &str) {
        if let Some(d) = std::env::var_os("ISB_TUI_DUMP") {
            std::fs::write(std::path::Path::new(&d).join(format!("{name}.txt")), s).unwrap();
        }
    }

    #[test]
    fn screens_render() {
        let mut a = app();
        a.focus = Focus::Replicas;
        a.rep = 2;
        let s = render(&a, 140, 40);
        dump("stack-140x40", &s);
        assert!(s.contains("e2e"), "{s}");
        assert!(s.contains("rolling out"), "{s}");
        assert!(s.contains("ConnectionRefusedError"), "{s}");
        assert!(s.contains("restarting its app"), "{s}");
        assert!(s.contains("disk ") && s.contains("588G/902G"), "{s}");
        assert!(s.contains("DISK") && s.contains("411M"), "{s}");

        a.focus = Focus::Sidebar;
        a.sel = 2;
        let s = render(&a, 100, 30);
        dump("sandbox-100x30", &s);
        assert!(s.contains("build-box"), "{s}");
        assert!(s.contains("system container"), "{s}");
        assert!(s.contains("12G"), "{s}");

        a.sel = 1;
        let s = render(&a, 80, 24);
        dump("failing-80x24", &s);
        assert!(s.contains("not found locally"), "{s}");

        a.sel = 0;
        a.mode = Mode::Logs(LogView {
            target: LogTarget::Service {
                stack: "e2e".into(),
                service: "web".into(),
            },
            lines: vec![
                LogLine {
                    slot: 1,
                    time: "2026-10-03T01:13:02+0000".into(),
                    text: "GET / HTTP/1.1 200".into(),
                },
                LogLine {
                    slot: 3,
                    time: "2026-10-03T01:13:03+0000".into(),
                    text: "Traceback (most recent call last):".into(),
                },
            ],
            follow: true,
            wrap: false,
            scroll: 0,
            only: None,
            loading: false,
            error: None,
            fetched: None,
        });
        let s = render(&a, 100, 20);
        dump("logs-100x20", &s);
        assert!(s.contains("following"), "{s}");

        a.mode = Mode::Help;
        dump("help-100x30", &render(&a, 100, 30));
        a.mode = Mode::Palette("dep".into());
        dump("palette-100x30", &render(&a, 100, 30));

        let mut empty = App::new(Theme::mono(), false);
        empty.set_overview(Overview::default());
        let s = render(&empty, 100, 24);
        dump("empty-direct-100x24", &s);
        assert!(s.contains("isb serve install"), "{s}");
        // Tiny terminals must not panic.
        render(&a, 20, 6);
        render(&app(), 30, 10);
    }
}
