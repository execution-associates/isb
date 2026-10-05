//! The log view: its lines, the replica filter and the search, and what
//! keys do while it is open.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::app::{App, Effect, Mode};

#[derive(Debug, Clone, PartialEq)]
pub enum LogTarget {
    Service { stack: String, service: String },
    Sandbox { name: String, oci: bool },
}

impl LogTarget {
    pub fn title(&self) -> String {
        match self {
            LogTarget::Service { stack, service } => format!("{stack}/{service}"),
            LogTarget::Sandbox { name, .. } => name.clone(),
        }
    }
}

/// One line of a log view, with the replica slot it came from (0: none).
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub slot: u32,
    pub time: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct LogView {
    pub target: LogTarget,
    pub lines: Vec<LogLine>,
    pub follow: bool,
    pub wrap: bool,
    /// Lines up from the bottom.
    pub scroll: usize,
    /// Show only this slot.
    pub only: Option<u32>,
    /// The search the view highlights and `n`/`N` step through.
    pub query: String,
    /// A search being typed after `/`.
    pub typing: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub fetched: Option<Instant>,
}

impl LogView {
    /// The lines on screen: all of them, or one replica's.
    pub fn shown(&self) -> Vec<&LogLine> {
        self.lines
            .iter()
            .filter(|l| self.only.is_none_or(|o| l.slot == o))
            .collect()
    }

    /// The search to highlight: the one being typed, else the kept one.
    pub fn pattern(&self) -> &str {
        self.typing.as_deref().unwrap_or(&self.query)
    }

    /// Scroll so the nearest match strictly older (`back`) or newer than
    /// the bottom line sits on the bottom line; `here` lets the bottom line
    /// itself count. False when there is none in that direction.
    fn seek(&mut self, back: bool, here: bool) -> bool {
        let shown = self.shown();
        let n = shown.len();
        if n == 0 || self.query.is_empty() {
            return false;
        }
        let cur = n - 1 - self.scroll.min(n - 1);
        let hit = |i: &usize| find_matches(&shown[*i].text, &self.query).next().is_some();
        let found = if back {
            let end = if here { cur + 1 } else { cur };
            (0..end).rev().find(hit)
        } else {
            let start = if here { cur } else { cur + 1 };
            (start..n).find(hit)
        };
        match found {
            Some(i) => {
                self.follow = false;
                self.scroll = n - 1 - i;
                true
            }
            None => false,
        }
    }
}

/// Byte ranges of `q` in `text`. Smart case: a query with a capital letter
/// matches case, otherwise case is ignored. ASCII folding keeps the byte
/// offsets of the lowered text valid in the original.
pub fn find_matches<'a>(text: &'a str, q: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    let fold = !q.chars().any(char::is_uppercase);
    let (hay, needle) = if fold {
        (text.to_ascii_lowercase(), q.to_ascii_lowercase())
    } else {
        (text.to_string(), q.to_string())
    };
    let len = needle.len();
    let starts: Vec<usize> = if len == 0 {
        Vec::new()
    } else {
        hay.match_indices(&needle).map(|(i, _)| i).collect()
    };
    starts.into_iter().map(move |i| (i, i + len))
}

impl App {
    pub(super) fn key_logs(&mut self, k: KeyEvent, mut v: LogView) -> Effect {
        if let Some(mut text) = v.typing.take() {
            match k.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    v.query = text;
                    if !v.query.is_empty() && !v.seek(true, true) && !v.seek(false, true) {
                        self.toast("warn", format!("no match for {:?}", v.query));
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    v.typing = Some(text);
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    v.typing = Some(text);
                }
                _ => v.typing = Some(text),
            }
            self.mode = Mode::Logs(v);
            return Effect::None;
        }
        match k.code {
            KeyCode::Esc if !v.query.is_empty() => v.query.clear(),
            KeyCode::Esc | KeyCode::Char('q') => return Effect::None,
            KeyCode::Char('/') => v.typing = Some(String::new()),
            KeyCode::Char(c @ ('n' | 'N')) if !v.query.is_empty() => {
                // Logs are read from the bottom, so n goes back in time.
                if !v.seek(c == 'n', false) {
                    let way = if c == 'n' { "older" } else { "newer" };
                    self.toast("info", format!("no {way} match"));
                }
            }
            KeyCode::Char('f') => {
                v.follow = !v.follow;
                v.scroll = 0;
            }
            KeyCode::Char('w') => v.wrap = !v.wrap,
            KeyCode::Char('0') | KeyCode::Char('a') => v.only = None,
            KeyCode::Char(c @ '1'..='9') => v.only = c.to_digit(10),
            KeyCode::Up | KeyCode::Char('k') => {
                v.follow = false;
                v.scroll += 1;
            }
            KeyCode::Down | KeyCode::Char('j') => v.scroll = v.scroll.saturating_sub(1),
            KeyCode::PageUp => {
                v.follow = false;
                v.scroll += 20;
            }
            KeyCode::PageDown => v.scroll = v.scroll.saturating_sub(20),
            KeyCode::Char('G') | KeyCode::End => {
                v.scroll = 0;
                v.follow = true;
            }
            KeyCode::Char('R') => {
                let t = v.target.clone();
                v.loading = true;
                self.mode = Mode::Logs(v);
                return Effect::Logs(t);
            }
            _ => {}
        }
        let lines = v.shown().len();
        v.scroll = v.scroll.min(lines.saturating_sub(1));
        self.mode = Mode::Logs(v);
        Effect::None
    }

    /// Log lines arrived for the open view.
    pub fn logs_loaded(&mut self, target: &LogTarget, r: Result<Vec<LogLine>, String>) {
        if let Mode::Logs(v) = &mut self.mode {
            if v.target != *target {
                return;
            }
            v.loading = false;
            v.fetched = Some(Instant::now());
            match r {
                Ok(lines) => {
                    // Keep the reader's place when not following.
                    if !v.follow && lines.len() >= v.lines.len() {
                        v.scroll += lines.len() - v.lines.len();
                    }
                    v.lines = lines;
                    v.error = None;
                }
                Err(e) => v.error = Some(e),
            }
        }
    }
}

/// Merge each replica's journal into one time-ordered view. Journal lines
/// (`short-iso`) start with a timestamp, host and unit, which are split off;
/// a console log has no timestamps and keeps its order.
pub fn merge_logs(
    by_instance: Vec<(String, String)>,
    slot_of: &dyn Fn(&str) -> u32,
) -> Vec<LogLine> {
    let mut out: Vec<LogLine> = Vec::new();
    for (inst, text) in by_instance {
        let slot = slot_of(&inst);
        for l in text.lines() {
            if l.starts_with("-- ") {
                continue;
            }
            out.push(parse_journal_line(l, slot));
        }
    }
    // Stable: lines without a time stay in their own order.
    out.sort_by(|a, b| a.time.cmp(&b.time));
    out
}

fn parse_journal_line(l: &str, slot: u32) -> LogLine {
    // 2026-10-03T01:13:02+0000 host isb-web[123]: message
    let mut parts = l.splitn(3, ' ');
    let (Some(ts), Some(_host), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return LogLine {
            slot,
            time: String::new(),
            text: l.to_string(),
        };
    };
    let looks_like_time = ts.len() >= 19 && ts.as_bytes().get(10) == Some(&b'T');
    if !looks_like_time {
        return LogLine {
            slot,
            time: String::new(),
            text: l.to_string(),
        };
    }
    let text = rest.split_once(": ").map(|(_, m)| m).unwrap_or(rest);
    LogLine {
        slot,
        time: ts.to_string(),
        text: text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn searches_logs() {
        let mut a = App::new(Theme::mono(), true);
        let target = LogTarget::Service {
            stack: "e2e".into(),
            service: "web".into(),
        };
        a.mode = Mode::Logs(LogView {
            target: target.clone(),
            lines: Vec::new(),
            follow: true,
            wrap: false,
            scroll: 0,
            only: None,
            query: String::new(),
            typing: None,
            loading: true,
            error: None,
            fetched: None,
        });
        let line = |slot, text: &str| LogLine {
            slot,
            time: String::new(),
            text: text.into(),
        };
        a.logs_loaded(
            &target,
            Ok(vec![
                line(1, "boot"),
                line(1, "Error: one"),
                line(2, "ok"),
                line(1, "error: two"),
                line(2, "tail"),
            ]),
        );
        let scroll = |a: &App| match &a.mode {
            Mode::Logs(v) => (v.scroll, v.follow),
            _ => panic!("left the logs"),
        };
        a.key(key('/'));
        for c in "error".chars() {
            a.key(key(c));
        }
        // Typing does not move the view; enter lands on the newest match.
        assert_eq!(scroll(&a), (0, true));
        a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(scroll(&a), (1, false));
        a.key(key('n'));
        assert_eq!(scroll(&a), (3, false));
        a.key(key('n'));
        assert_eq!(scroll(&a), (3, false), "no older match stays put");
        a.key(key('N'));
        assert_eq!(scroll(&a), (1, false));
        // A capital letter matches case; replica 2 has none.
        a.key(key('/'));
        a.key(key('E'));
        a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(scroll(&a), (3, false));
        a.key(key('2'));
        a.key(key('/'));
        a.key(key('a'));
        a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(scroll(&a), (0, false), "the match in replica 2 only");
        // esc clears the search first, then leaves.
        a.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(&a.mode, Mode::Logs(v) if v.query.is_empty()));
        a.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(a.mode, Mode::Normal));
    }

    #[test]
    fn finds_matches() {
        let m = |t, q| find_matches(t, q).collect::<Vec<_>>();
        assert_eq!(m("Error error", "error"), vec![(0, 5), (6, 11)]);
        assert_eq!(m("Error error", "Error"), vec![(0, 5)]);
        assert_eq!(m("héllo", "llo"), vec![(3, 6)]);
        assert!(m("x", "").is_empty());
    }

    #[test]
    fn merges_journals() {
        let lines = merge_logs(
            vec![
                (
                    "a".into(),
                    "2026-10-03T01:00:02+0000 h isb-web[1]: two\n".into(),
                ),
                (
                    "b".into(),
                    "-- No entries --\n2026-10-03T01:00:01+0000 h isb-web[2]: one\n".into(),
                ),
            ],
            &|n| if n == "a" { 1 } else { 2 },
        );
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].slot, lines[0].text.as_str()), (2, "one"));
        assert_eq!((lines[1].slot, lines[1].text.as_str()), (1, "two"));
    }
}
