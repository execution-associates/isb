//! Colour tokens and the status vocabulary. A web UI should use the same
//! names: `ok`, `warn`, `err`, `info`, `accent`, `fg`, `dim`, `faint`.
//!
//! Glyphs carry the meaning and colour only adds to it, so the dashboard
//! reads the same with `NO_COLOR` set.
//!
//! Every colour is one of the terminal's own sixteen ANSI colours or its
//! default foreground, never an RGB value, so the dashboard follows whatever
//! theme the terminal has, light or dark. Dim text uses the DIM attribute,
//! which the terminal blends toward its own background.

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
    pub color: bool,
}

impl Theme {
    pub fn detect() -> Theme {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Theme::mono();
        }
        Theme {
            fg: Color::Reset,
            dim: Color::Reset,
            // Bright black: the palette slot themes reserve for muted text.
            faint: Color::DarkGray,
            accent: Color::Blue,
            ok: Color::Green,
            warn: Color::Yellow,
            err: Color::Red,
            info: Color::Magenta,
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
            color: false,
        }
    }

    pub fn fg(&self) -> Style {
        Style::default().fg(self.fg)
    }
    pub fn dim(&self) -> Style {
        let s = Style::default().fg(self.dim);
        if self.color {
            s.add_modifier(Modifier::DIM)
        } else {
            s
        }
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
        Style::default().fg(self.dim).add_modifier(Modifier::BOLD)
    }
    /// Reversed video rather than a background colour: no palette slot is
    /// guaranteed to contrast with the default foreground on every theme.
    pub fn selected(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }
    pub fn level(&self, level: &str) -> Style {
        match level {
            "error" => Style::default().fg(self.err),
            "warn" => Style::default().fg(self.warn),
            _ => self.dim(),
        }
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
            "stopped" | "retired" | "inactive" => ("○", self.dim()),
            "frozen" => ("❄", s(self.info)),
            _ => ("·", s(self.faint)),
        }
    }
}
