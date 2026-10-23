//! The Timers page: the region's timetable (za2_core::schedule) as rows, the ones on now first, then by start.

use slint::Color;
use za2_core::schedule::{self, Rule, Schedule, TimedEvent};

use crate::TimerRow;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Colour of a kind of event (the dot in front of its row).
fn kind_color(kind: &str) -> Color {
    match kind {
        "reset" => Color::from_rgb_u8(0x7f, 0xb4, 0xff),
        "boss" => Color::from_rgb_u8(0xf1, 0x7b, 0x87),
        "pvp" => Color::from_rgb_u8(0xff, 0xa8, 0x5c),
        _ => Color::from_rgb_u8(0x68, 0xcb, 0xa4),
    }
}

/// How the rule reads: "Every hour at :30", "Wed 04:00".
fn describe(event: &TimedEvent) -> String {
    match &event.rule {
        Rule::Daily { at, .. } => format!("Daily {at}"),
        Rule::Weekly { days, at, .. } => {
            let names: Vec<&str> = days.iter().filter_map(|&d| WEEKDAYS.get(d as usize).copied()).collect();
            format!("{} {at}", names.join(", "))
        }
        Rule::Hourly { minute, .. } => format!("Every hour at :{minute:02}"),
        Rule::Every { hours, from, .. } => format!("Every {hours} h from {from}"),
        Rule::Windows { windows } => windows.iter().map(|w| format!("{}–{}", w.start, w.end)).collect::<Vec<_>>().join(", "),
    }
}

/// "1:02:03", "12:03", "45 s".
fn countdown(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{}:{:02}", s / 60, s % 60),
        _ if s < 86_400 => format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60),
        _ => format!("{} d {} h", s / 86_400, s / 3600 % 24),
    }
}

/// When a start is, in the region's clock: "21:00", "tomorrow 04:00", "Wed 04:00".
fn when(table: &Schedule, at: i64, now: i64) -> String {
    let (weekday, hour, minute, days) = table.local_time(at, now);
    match days {
        0 => format!("{hour:02}:{minute:02}"),
        1 => format!("tomorrow {hour:02}:{minute:02}"),
        _ => format!("{} {hour:02}:{minute:02}", WEEKDAYS[weekday as usize]),
    }
}

/// The page's rows at `now` (UTC unix ms).
pub fn rows(now: i64) -> Vec<TimerRow> {
    let table = schedule::get();
    let mut rows: Vec<(bool, i64, TimerRow)> = table
        .events
        .iter()
        .filter_map(|e| {
            let o = table.current_or_next(e, now)?;
            let on = o.is_on(now);
            let mut detail = describe(e);
            if !e.note.is_empty() {
                detail = format!("{detail} · {}", e.note);
            }
            let row = TimerRow {
                name: e.name.clone().into(),
                detail: detail.into(),
                status: if on { format!("on · ends in {}", countdown(o.end - now)) } else { format!("in {}", countdown(o.start - now)) }.into(),
                when: if on { "now".to_string() } else { when(table, o.start, now) }.into(),
                color: kind_color(&e.kind),
                on,
                unverified: !e.verified,
            };
            Some((!on, if on { o.end } else { o.start }, row))
        })
        .collect();
    rows.sort_by_key(|r| (r.0, r.1));
    rows.into_iter().map(|r| r.2).collect()
}

/// The region's clock now: "São Paulo · Thu 18:30:05".
pub fn clock(now: i64) -> String {
    let table = schedule::get();
    let (weekday, hour, minute, _) = table.local_time(now, now);
    format!("{} · {} {hour:02}:{minute:02}:{:02}", table.clock, WEEKDAYS[weekday as usize], now.rem_euclid(60_000) / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_read() {
        let table = schedule::get();
        let read = |id: &str| describe(table.events.iter().find(|e| e.id == id).unwrap());
        assert_eq!(read("weekly_reset"), "Wed 04:00");
        assert_eq!(read("spacetime_rift"), "Every 3 h from 02:00");
        assert_eq!(read("dimensional_invasion"), "Every hour at :30");
    }

    #[test]
    fn countdowns() {
        assert_eq!(countdown(45_000), "45 s");
        assert_eq!(countdown(605_000), "10:05");
        assert_eq!(countdown(3_723_000), "1:02:03");
        assert_eq!(countdown(2 * 86_400_000 + 3 * 3_600_000), "2 d 3 h");
    }
}
