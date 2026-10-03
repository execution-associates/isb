//! Cron schedules: the five-field form (`minute hour day-of-month month
//! day-of-week`) and the `@hourly`-style aliases, evaluated in UTC or a
//! fixed offset from it.
//!
//! Fields take `*`, numbers, ranges `a-b`, steps `*/n`, `a-b/n` and `a/n`
//! (from `a` to the field's end), and comma lists; months and weekdays take
//! names (`jan`, `mon`; any case). Day of week is 0-7, both 0 and 7 Sunday.
//! As in Vixie cron, when both day of month and day of week are restricted
//! a day matching either fires.
//!
//! Times are whole minutes in Unix seconds. There is no DST: a schedule's
//! offset is fixed, so every day has every minute exactly once.

use crate::error::{Error, Result};

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    /// Bits 1-31.
    doms: u32,
    /// Bits 1-12.
    months: u16,
    /// Bits 0-6 (Sunday 0).
    dows: u8,
    dom_star: bool,
    dow_star: bool,
    /// Seconds east of UTC the fields are read in.
    offset: i32,
    text: String,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Parse one field into a bitset of the values in `lo..=hi`. `star` is set
/// when the field is `*` or `*/1` (every value).
fn field(s: &str, lo: u32, hi: u32, names: &[&str], what: &str) -> Result<(u64, bool)> {
    let bad = |why: &str| Error::invalid(format!("cron {what} {s:?}: {why}"));
    let num = |t: &str| -> Result<u32> {
        if let Ok(n) = t.parse::<u32>() {
            return Ok(n);
        }
        let l = t.to_ascii_lowercase();
        names
            .iter()
            .position(|n| *n == l)
            .map(|i| i as u32 + if names.len() == 12 { 1 } else { 0 })
            .ok_or_else(|| bad(&format!("{t:?} is not a number or a name")))
    };
    let mut bits = 0u64;
    let mut star = false;
    for part in s.split(',') {
        if part.is_empty() {
            return Err(bad("empty list item"));
        }
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => {
                let n: u32 = st
                    .parse()
                    .map_err(|_| bad(&format!("step {st:?} is not a number")))?;
                if n == 0 {
                    return Err(bad("a step of 0"));
                }
                (r, Some(n))
            }
            None => (part, None),
        };
        let (a, b) = if range == "*" {
            if step.unwrap_or(1) == 1 {
                star = true;
            }
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (num(a)?, num(b)?)
        } else {
            let a = num(range)?;
            // `a/n` runs from a to the field's end.
            (a, if step.is_some() { hi } else { a })
        };
        if a < lo || b > hi || a > b {
            return Err(bad(&format!("{a}-{b} is outside {lo}-{hi}")));
        }
        let step = step.unwrap_or(1);
        let mut v = a;
        while v <= b {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, star))
}

/// Parse a fixed UTC offset: `UTC`, `Z`, `+05:30`, `-0800`, `+2`.
pub fn parse_offset(s: &str) -> Result<i32> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("utc") || t == "Z" {
        return Ok(0);
    }
    let bad = || {
        Error::invalid(format!(
            "timezone {s:?}: UTC or a fixed offset such as +02:00 (named zones are not supported)"
        ))
    };
    let t = t
        .strip_prefix("UTC")
        .or_else(|| t.strip_prefix("utc"))
        .unwrap_or(t);
    let (sign, rest) = match t.as_bytes().first() {
        Some(b'+') => (1, &t[1..]),
        Some(b'-') => (-1, &t[1..]),
        _ => return Err(bad()),
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None if rest.len() == 4 => rest.split_at(2),
        None => (rest, "0"),
    };
    let h: i32 = h.parse().map_err(|_| bad())?;
    let m: i32 = m.parse().map_err(|_| bad())?;
    if h > 14 || m > 59 {
        return Err(bad());
    }
    Ok(sign * (h * 3600 + m * 60))
}

impl Schedule {
    /// Parse `expr` (five fields or an alias), read in UTC.
    pub fn parse(expr: &str) -> Result<Schedule> {
        Schedule::parse_in(expr, 0)
    }

    /// Parse `expr`, its fields read at `offset` seconds east of UTC.
    pub fn parse_in(expr: &str, offset: i32) -> Result<Schedule> {
        let text = expr.trim();
        let expanded = match text.to_ascii_lowercase().as_str() {
            "@yearly" | "@annually" => "0 0 1 1 *",
            "@monthly" => "0 0 1 * *",
            "@weekly" => "0 0 * * 0",
            "@daily" | "@midnight" => "0 0 * * *",
            "@hourly" => "0 * * * *",
            a if a.starts_with('@') => {
                return Err(Error::invalid(format!(
                    "cron {text:?}: aliases are @yearly, @monthly, @weekly, @daily and @hourly"
                )));
            }
            _ => text,
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return Err(Error::invalid(format!(
                "cron {text:?}: five fields (minute hour day-of-month month day-of-week) or an alias such as @daily"
            )));
        }
        let (minutes, _) = field(f[0], 0, 59, &[], "minute")?;
        let (hours, _) = field(f[1], 0, 23, &[], "hour")?;
        let (doms, dom_star) = field(f[2], 1, 31, &[], "day of month")?;
        let (months, _) = field(f[3], 1, 12, &MONTHS, "month")?;
        let (mut dows, dow_star) = field(f[4], 0, 7, &DAYS, "day of week")?;
        if dows & (1 << 7) != 0 {
            dows = (dows | 1) & 0x7f;
        }
        let s = Schedule {
            minutes,
            hours: hours as u32,
            doms: doms as u32,
            months: months as u16,
            dows: dows as u8,
            dom_star,
            dow_star,
            offset,
            text: text.to_string(),
        };
        // `30 2 31 2 *` parses and never fires: say so now.
        if s.next_after(946_684_800).is_none() {
            return Err(Error::invalid(format!("cron {text:?}: never fires")));
        }
        Ok(s)
    }

    /// The expression as given.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn day_matches(&self, y: i64, m: u32, d: u32) -> bool {
        if self.months & (1 << m) == 0 {
            return false;
        }
        let dom = self.doms & (1 << d) != 0;
        let dow = self.dows & (1 << weekday(y, m, d)) != 0;
        match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (false, true) => dom,
            (true, false) => dow,
            (false, false) => dom || dow,
        }
    }

    /// The first firing time strictly after `t` (Unix seconds), or `None`
    /// if none comes within eight years (the schedule never fires).
    pub fn next_after(&self, t: i64) -> Option<i64> {
        let local = t + self.offset as i64;
        // The next whole minute after t.
        let start = local.div_euclid(60) * 60 + 60;
        let first_day = start.div_euclid(86_400);
        // Eight years covers every leap-day and weekday combination.
        for day in first_day..first_day + 366 * 8 {
            let (y, m, d) = civil_from_days(day);
            if !self.day_matches(y, m, d) {
                continue;
            }
            let from = if day == first_day {
                start.rem_euclid(86_400) / 60
            } else {
                0
            };
            for mm in from..1440 {
                let (h, mi) = (mm / 60, mm % 60);
                if self.hours & (1 << h) != 0 && self.minutes & (1 << mi) != 0 {
                    return Some(day * 86_400 + mm * 60 - self.offset as i64);
                }
            }
        }
        None
    }
}

impl std::fmt::Display for Schedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// (year, month 1-12, day 1-31) of a day number (days since 1970-01-01).
/// Howard Hinnant's algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since 1970-01-01 of a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = Sunday.
fn weekday(y: i64, m: u32, d: u32) -> u32 {
    // 1970-01-01 was a Thursday.
    (days_from_civil(y, m, d) + 4).rem_euclid(7) as u32
}

/// `YYYYMMDDTHHMMSSZ` for Unix seconds `t` (UTC).
pub fn compact_utc(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix seconds `t`.
pub fn rfc3339(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// Parse [`compact_utc`] back to Unix seconds.
pub fn parse_compact_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(4..6)?, n(6..8)?);
    let (h, mi, se) = (n(9..11)?, n(11..13)?, n(13..15)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix seconds of a UTC civil time.
    fn at(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60
    }

    fn next(expr: &str, t: i64) -> i64 {
        Schedule::parse(expr).unwrap().next_after(t).unwrap()
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for z in [-1000, 0, 10_957, 11_016, 11_017, 19_782, 20_000, 60_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(weekday(1970, 1, 1), 4);
        assert_eq!(weekday(2026, 10, 3), 6, "a Saturday");
        assert_eq!(compact_utc(at(2026, 10, 3, 4, 5) + 6), "20261003T040506Z");
        assert_eq!(
            parse_compact_utc("20261003T040506Z"),
            Some(at(2026, 10, 3, 4, 5) + 6)
        );
        assert_eq!(parse_compact_utc("20261003T0405Z"), None);
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn every_minute_and_steps() {
        let t = at(2026, 10, 3, 12, 0);
        assert_eq!(next("* * * * *", t), t + 60);
        // Strictly after: from 12:00:30 the next is 12:01.
        assert_eq!(next("* * * * *", t + 30), t + 60);
        assert_eq!(next("*/15 * * * *", t), at(2026, 10, 3, 12, 15));
        assert_eq!(
            next("*/15 * * * *", at(2026, 10, 3, 12, 50)),
            at(2026, 10, 3, 13, 0)
        );
        assert_eq!(next("5/20 * * * *", t), at(2026, 10, 3, 12, 5));
        assert_eq!(
            next("5/20 * * * *", at(2026, 10, 3, 12, 45)),
            at(2026, 10, 3, 13, 5)
        );
        assert_eq!(
            next("10-20/5 * * * *", at(2026, 10, 3, 12, 16)),
            at(2026, 10, 3, 12, 20)
        );
        assert_eq!(next("0,30 */6 * * *", t), at(2026, 10, 3, 12, 30));
        assert_eq!(
            next("0,30 */6 * * *", at(2026, 10, 3, 12, 30)),
            at(2026, 10, 3, 18, 0)
        );
        assert_eq!(
            next("59 23 * * *", at(2026, 12, 31, 23, 59)),
            at(2027, 1, 1, 23, 59)
        );
    }

    #[test]
    fn aliases() {
        let t = at(2026, 10, 3, 12, 34);
        assert_eq!(next("@hourly", t), at(2026, 10, 3, 13, 0));
        assert_eq!(next("@daily", t), at(2026, 10, 4, 0, 0));
        assert_eq!(next("@midnight", t), at(2026, 10, 4, 0, 0));
        assert_eq!(
            next("@weekly", t),
            at(2026, 10, 4, 0, 0),
            "the 4th is a Sunday"
        );
        assert_eq!(next("@monthly", t), at(2026, 11, 1, 0, 0));
        assert_eq!(next("@yearly", t), at(2027, 1, 1, 0, 0));
        assert_eq!(next("@annually", t), at(2027, 1, 1, 0, 0));
        assert!(Schedule::parse("@reboot").is_err());
    }

    #[test]
    fn month_ends_and_leap_years() {
        // The 31st skips months without one.
        assert_eq!(
            next("0 0 31 * *", at(2026, 4, 1, 0, 0)),
            at(2026, 5, 31, 0, 0)
        );
        assert_eq!(
            next("0 0 31 * *", at(2026, 5, 31, 0, 0)),
            at(2026, 7, 31, 0, 0)
        );
        // February 29th: the next leap year.
        assert_eq!(
            next("0 12 29 2 *", at(2026, 3, 1, 0, 0)),
            at(2028, 2, 29, 12, 0)
        );
        // 2100 is not a leap year; 2000 was.
        assert_eq!(
            next("0 0 29 2 *", at(2096, 3, 1, 0, 0)),
            at(2104, 2, 29, 0, 0)
        );
        assert_eq!(
            next("0 0 29 2 *", at(1999, 1, 1, 0, 0)),
            at(2000, 2, 29, 0, 0)
        );
        // The last day of February, any year: 28 or 29.
        assert_eq!(
            next("0 0 28,29 2 *", at(2026, 2, 28, 0, 0)),
            at(2027, 2, 28, 0, 0)
        );
        // Year end.
        assert_eq!(
            next("0 0 1 1 *", at(2026, 12, 31, 23, 59)),
            at(2027, 1, 1, 0, 0)
        );
        // Impossible dates are refused, not looped over.
        assert!(Schedule::parse("0 0 30 2 *").is_err());
        assert!(Schedule::parse("0 0 31 4,6,9,11 *").is_err());
    }

    #[test]
    fn days_of_week_and_names() {
        let sat = at(2026, 10, 3, 9, 0);
        assert_eq!(next("0 9 * * mon-fri", sat), at(2026, 10, 5, 9, 0));
        assert_eq!(next("0 9 * * 1-5", sat), at(2026, 10, 5, 9, 0));
        // 7 and 0 are both Sunday.
        assert_eq!(next("0 9 * * 7", sat), at(2026, 10, 4, 9, 0));
        assert_eq!(next("0 9 * * SUN", sat), at(2026, 10, 4, 9, 0));
        assert_eq!(next("0 0 1 JAN-mar *", sat), at(2027, 1, 1, 0, 0));
        // Both restricted: either matches (Vixie cron). The 13th or a Friday.
        assert_eq!(next("0 0 13 * 5", sat), at(2026, 10, 9, 0, 0));
        assert_eq!(
            next("0 0 13 * 5", at(2026, 10, 9, 0, 0)),
            at(2026, 10, 13, 0, 0)
        );
        // A restricted day of week with `*` day of month: weekday only.
        assert_eq!(
            next("0 0 * * 5", at(2026, 10, 9, 0, 0)),
            at(2026, 10, 16, 0, 0)
        );
    }

    #[test]
    fn errors() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
            "1,,2 * * * *",
            "* * * foo *",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad:?}");
        }
        let e = Schedule::parse("61 * * * *").unwrap_err().to_string();
        assert!(e.contains("minute"), "{e}");
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("UTC").unwrap(), 0);
        assert_eq!(parse_offset("").unwrap(), 0);
        assert_eq!(parse_offset("+02:00").unwrap(), 7200);
        assert_eq!(parse_offset("-0830").unwrap(), -30_600);
        assert_eq!(parse_offset("UTC+5").unwrap(), 18_000);
        assert!(parse_offset("Europe/Berlin").is_err());
        assert!(parse_offset("+25:00").is_err());
        // 09:00 at +02:00 is 07:00 UTC.
        let s = Schedule::parse_in("0 9 * * *", 7200).unwrap();
        assert_eq!(
            s.next_after(at(2026, 10, 3, 0, 0)).unwrap(),
            at(2026, 10, 3, 7, 0)
        );
        // 01:00 at +02:00 is 23:00 UTC the day before.
        let s = Schedule::parse_in("0 1 * * *", 7200).unwrap();
        assert_eq!(
            s.next_after(at(2026, 10, 3, 0, 0)).unwrap(),
            at(2026, 10, 3, 23, 0)
        );
        // A day-of-week restriction is read in local time too.
        let s = Schedule::parse_in("30 0 * * 0", -3600).unwrap();
        assert_eq!(
            s.next_after(at(2026, 10, 3, 0, 0)).unwrap(),
            at(2026, 10, 4, 1, 30)
        );
    }
}
