//! Names isb derives: instance names it accepts, and deterministic device
//! names for guest mount paths.

use crate::error::{Error, Result};

/// Valid incus instance name: <= 63 chars, [a-zA-Z0-9-], starts with a letter,
/// does not end with '-'.
pub fn validate_instance_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && !name.ends_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "invalid sandbox name {name:?}: use at most 63 of [a-z0-9-], starting with a letter"
        )))
    }
}

/// Deterministic device name for a guest mount path.
pub fn device_name_for_path(guest: &str) -> String {
    let mut s = String::new();
    for c in guest.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-').to_string();
    let s = if s.is_empty() { "mount".to_string() } else { s };
    if s.len() <= 48 {
        return s;
    }
    format!(
        "{}-{:08x}",
        s[s.len() - 39..].trim_start_matches('-'),
        fnv32(guest)
    )
}

fn fnv32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}
