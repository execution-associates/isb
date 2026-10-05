//! Mouse input: a click picks a sidebar item, the wheel steps the focus.

use ratatui::layout::Rect;

use super::app::{App, Focus, Item, Mode};

impl App {
    /// Pick sidebar item `n`, as a click on it does.
    pub fn select(&mut self, n: usize) {
        if !matches!(self.mode, Mode::Normal) || n >= self.items().len() {
            return;
        }
        if n != self.sel {
            self.sel = n;
            self.svc = 0;
            self.rep = 0;
        }
        self.focus = Focus::Sidebar;
    }

    /// The mouse wheel moves whatever has focus, as the arrow keys do.
    pub fn wheel(&mut self, delta: isize) {
        if matches!(self.mode, Mode::Normal) {
            self.step(delta);
            self.clamp();
        }
    }
}

/// The sidebar item drawn at a screen cell, when the terminal is `area`.
pub fn sidebar_hit(app: &App, area: Rect, col: u16, row: u16) -> Option<usize> {
    let [_, side, ..] = super::ui::regions(area);
    let inner = super::ui::pane(app, "", false).inner(side);
    if col < inner.x || col >= inner.right() || row < inner.y || row >= inner.bottom() {
        return None;
    }
    let (owners, sel_line) = sidebar_rows(app);
    let scroll = sidebar_scroll(sel_line, inner.height);
    owners
        .get(scroll + (row - inner.y) as usize)
        .copied()
        .flatten()
}

/// Which item each sidebar line shows (`None` for headings and gaps), and
/// the selected item's line.
fn sidebar_rows(app: &App) -> (Vec<Option<usize>>, usize) {
    let mut owners = Vec::new();
    let mut sel_line = 0;
    let mut section = None;
    for (n, it) in app.items().iter().enumerate() {
        let sec = matches!(it, Item::Stack(_));
        if section != Some(sec) {
            if section.is_some() {
                owners.push(None);
            }
            owners.push(None);
            section = Some(sec);
        }
        if n == app.sel {
            sel_line = owners.len();
        }
        owners.push(Some(n));
    }
    (owners, sel_line)
}

pub(super) fn sidebar_scroll(sel_line: usize, height: u16) -> usize {
    sel_line.saturating_sub((height as usize).saturating_sub(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::{Overview, Sandbox, Stack};
    use crate::tui::theme::Theme;

    #[test]
    fn clicking_the_sidebar_selects() {
        let mut a = App::new(Theme::mono(), true);
        a.set_overview(Overview {
            stacks: vec![Stack {
                name: "e2e".into(),
                ..Default::default()
            }],
            sandboxes: vec![Sandbox {
                name: "box".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        // 120x40: the sidebar's inner area starts at column 2, row 4, under
        // the STACKS heading; SANDBOXES follows a blank line.
        let area = Rect::new(0, 0, 120, 40);
        let hit = |a: &App, col, row| sidebar_hit(a, area, col, row);
        assert_eq!(hit(&a, 5, 4), None);
        assert_eq!(hit(&a, 5, 5), Some(0));
        assert_eq!(hit(&a, 5, 6), None);
        assert_eq!(hit(&a, 5, 8), Some(1));
        assert_eq!(hit(&a, 60, 8), None);
        a.select(1);
        assert_eq!(a.selected(), Some(Item::Sandbox(0)));
        assert_eq!(a.focus, Focus::Sidebar);
        a.wheel(-1);
        assert_eq!(a.selected(), Some(Item::Stack(0)));
    }
}
