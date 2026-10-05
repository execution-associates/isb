//! The header: host, version, counts, and the host's cpu, memory and disk.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::App;
use super::fmt::{bar, bytes, spark, trunc};
use super::model::Host;
use super::theme::Theme;

pub(super) fn header(f: &mut Frame, app: &App, area: Rect) {
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
    let mut left = Line::from(vec![
        Span::styled(" isb", t.accent().add_modifier(Modifier::BOLD)),
        Span::styled(" · ", t.faint()),
        Span::styled(host.to_string(), t.bold()),
    ]);
    let [l1, l2, rule] = Layout::vertical([Constraint::Length(1); 3]).areas(area);
    let room =
        (l1.width as usize).saturating_sub(left.width() + Line::from(right.clone()).width() + 3);
    if app.updated.is_some() {
        left.spans.extend(gauges(t, h, room));
    }
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
    if area.width >= 80 {
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

/// The host's cpu, memory and disk, as wide as `room` allows: bars shrink
/// first, then disk and memory fall away. An older daemon sends no storage
/// numbers, so disk is left out.
fn gauges<'a>(t: &Theme, h: &Host, room: usize) -> Vec<Span<'a>> {
    let frac = |used: u64, total: u64| {
        if total > 0 {
            used as f64 / total as f64
        } else {
            0.0
        }
    };
    let build = |spark_w: usize, bar_w: usize, mem: bool, disk: bool| {
        let mut v = vec![
            Span::styled("   cpu ", t.dim()),
            Span::styled(spark(&h.cpu_history, spark_w, Some(100.0)), t.accent()),
            Span::styled(format!(" {:>3.0}%", h.cpu_pct.unwrap_or(0.0)), t.bold()),
        ];
        let mut meter = |label: &'static str, used: u64, total: u64| {
            v.push(Span::styled(label, t.dim()));
            if bar_w > 0 {
                v.push(Span::styled(bar(frac(used, total), bar_w), t.accent()));
                v.push(Span::raw(" "));
            }
            v.push(Span::styled(
                format!("{}/{}", bytes(used), bytes(total)),
                t.bold(),
            ));
        };
        if mem {
            meter("   mem ", h.mem_used, h.mem_total);
        }
        if disk {
            meter("   disk ", h.disk_used, h.disk_total);
        }
        v
    };
    let disk = h.disk_total > 0;
    [
        (12, 10, true, disk),
        (8, 6, true, disk),
        (6, 0, true, disk),
        (6, 0, true, false),
        (6, 0, false, false),
    ]
    .into_iter()
    .map(|(s, b, m, d)| build(s, b, m, d))
    .find(|v| Line::from(v.clone()).width() <= room)
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &[Span]) -> String {
        v.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn gauges_shrink_before_dropping_disk() {
        let t = Theme::mono();
        let h = Host {
            cpu_pct: Some(29.0),
            mem_used: 54 << 30,
            mem_total: 252 << 30,
            disk_used: 1 << 40,
            disk_total: 4 << 40,
            ..Default::default()
        };
        // A 105-column terminal leaves about 70 beside the host and version.
        let narrow = text(&gauges(&t, &h, 70));
        assert!(narrow.contains("disk"), "{narrow}");
        assert!(text(&gauges(&t, &h, 200)).contains('█'));
        assert!(!text(&gauges(&t, &h, 30)).contains("disk"));
        assert!(gauges(&t, &h, 5).is_empty());
        let old = Host { disk_total: 0, ..h };
        assert!(!text(&gauges(&t, &old, 200)).contains("disk"));
    }
}
