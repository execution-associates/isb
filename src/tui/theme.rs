//! Colour tokens and the status vocabulary. A web UI should use the same
//! names: `ok`, `warn`, `err`, `info`, `accent`, `fg`, `dim`, `faint`.
//!
//! Glyphs carry the meaning and colour only adds to it, so the dashboard
//! reads the same with `NO_COLOR` set.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub fg: Color,
    pub dim: Color,
    pub faint: Color,
    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    pub info: Color,
    pub sel_bg: Color,
    pub color: bool,
}

impl Theme {
    pub fn detect() -> Theme {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Theme::mono();
        }
        Theme {
            fg: Color::Rgb(0xdd, 0xe3, 0xea),
            dim: Color::Rgb(0x8a, 0x94, 0xa3),
            faint: Color::Rgb(0x4a, 0x52, 0x5e),
            accent: Color::Rgb(0x7a, 0xc8, 0xff),
            ok: Color::Rgb(0x5f, 0xd3, 0x8d),
            warn: Color::Rgb(0xf2, 0xc1, 0x4e),
            err: Color::Rgb(0xff, 0x6b, 0x6b),
            info: Color::Rgb(0xa8, 0x9b, 0xff),
            sel_bg: Color::Rgb(0x23, 0x2b, 0x36),
            color: true,
        }
    }

    pub fn mono() -> Theme {
        Theme {
            fg: Color::Reset,
            dim: Color::Reset,
            faint: Color::Reset,
            accent: Color::Reset,
            ok: Color::Reset,
            warn: Color::Reset,
            err: Color::Reset,
            info: Color::Reset,
            sel_bg: Color::Reset,
            color: false,
        }
    }

    pub fn fg(&self) -> Style {
        Style::default().fg(self.fg)
    }
    pub fn dim(&self) -> Style {
        Style::default().fg(self.dim)
    }
    pub fn faint(&self) -> Style {
        Style::default().fg(self.faint)
    }
    pub fn accent(&self) -> Style {
        Style::default().fg(self.accent)
    }
    pub fn bold(&self) -> Style {
        self.fg().add_modifier(Modifier::BOLD)
    }
    /// A section heading: small caps by convention.
    pub fn heading(&self) -> Style {
        self.dim().add_modifier(Modifier::BOLD)
    }
    pub fn selected(&self) -> Style {
        if self.color {
            Style::default().bg(self.sel_bg)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }
    pub fn level(&self, level: &str) -> Style {
        Style::default().fg(match level {
            "error" => self.err,
            "warn" => self.warn,
            _ => self.dim,
        })
    }

    /// Glyph and style for a state word: instance status, health, service
    /// or rollout state.
    pub fn status(&self, state: &str) -> (&'static str, Style) {
        let s = |c: Color| Style::default().fg(c);
        match state.to_ascii_lowercase().as_str() {
            "healthy" | "converged" | "serving" | "running" | "active" => ("●", s(self.ok)),
            "none" => ("●", s(self.ok)),
            "updating" | "probing" | "monitoring" | "creating" | "draining" => ("◐", s(self.warn)),
            "starting" | "waiting" | "activating" => ("◌", s(self.warn)),
            "paused" => ("◫", s(self.warn)),
            "failing" | "unhealthy" | "failed" | "error" => ("✖", s(self.err)),
            "stopped" | "retired" | "inactive" => ("○", s(self.dim)),
            "frozen" => ("❄", s(self.info)),
            _ => ("·", s(self.faint)),
        }
    }
}
