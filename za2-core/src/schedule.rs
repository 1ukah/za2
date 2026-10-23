//! The timetable of the game's timed content (resets, field events, the Artifact Siege, ...) from
//! assets/schedule/timers.json. The times are those of one region's clock (`utcOffsetMinutes`, no daylight saving);
//! nothing in the game's traffic announces them, so they are a table.

use serde::Deserialize;
use std::sync::OnceLock;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub region: String,
    /// The name of the clock the times are in ("São Paulo").
    pub clock: String,
    pub utc_offset_minutes: i64,
    pub events: Vec<TimedEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimedEvent {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub note: String,
    pub rule: Rule,
}

/// When an event starts and how long it is on ("HH:MM" in the region's clock; days 0 = Sunday ... 6 = Saturday).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Rule {
    Daily { at: String, #[serde(default)] minutes: i64 },
    Weekly { days: Vec<u32>, at: String, #[serde(default)] minutes: i64 },
    Hourly { minute: i64, #[serde(default)] minutes: i64 },
    /// Every `hours` hours from `from` on, the day's grid starting again at midnight.
    Every { hours: i64, from: String, #[serde(default)] minutes: i64 },
    /// Open in these windows every day (an end at or before the start runs past midnight).
    Windows { windows: Vec<Window> },
}

#[derive(Deserialize)]
pub struct Window {
    pub start: String,
    pub end: String,
}

/// One occurrence, in UTC unix milliseconds. An instant (a reset) has `end == start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub start: i64,
    pub end: i64,
}

impl Occurrence {
    pub fn is_on(&self, now: i64) -> bool {
        self.start <= now && now < self.end
    }
}

pub fn get() -> &'static Schedule {
    static TABLE: OnceLock<Schedule> = OnceLock::new();
    TABLE.get_or_init(|| crate::assets::read_json("schedule/timers.json").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
}

/// "HH:MM" as milliseconds after midnight.
fn time_of_day(text: &str) -> i64 {
    let (h, m) = text.split_once(':').unwrap_or((text, "0"));
    h.trim().parse::<i64>().unwrap_or(0) * HOUR_MS + m.trim().parse::<i64>().unwrap_or(0) * MINUTE_MS
}

impl Rule {
    /// Starts (region-local milliseconds since the epoch) and lengths of the occurrences that start on local `day`
    /// (days since the epoch).
    fn on_day(&self, day: i64, out: &mut Vec<(i64, i64)>) {
        let midnight = day * DAY_MS;
        // 1970-01-01 was a Thursday
        let weekday = (day + 4).rem_euclid(7) as u32;
        match self {
            Rule::Daily { at, minutes } => out.push((midnight + time_of_day(at), minutes * MINUTE_MS)),
            Rule::Weekly { days, at, minutes } => {
                if days.contains(&weekday) {
                    out.push((midnight + time_of_day(at), minutes * MINUTE_MS));
                }
            }
            Rule::Hourly { minute, minutes } => out.extend((0..24).map(|h| (midnight + h * HOUR_MS + minute * MINUTE_MS, minutes * MINUTE_MS))),
            Rule::Every { hours, from, minutes } => {
                let step = hours.max(&1) * HOUR_MS;
                let mut at = time_of_day(from) % step;
                while at < DAY_MS {
                    out.push((midnight + at, minutes * MINUTE_MS));
                    at += step;
                }
            }
            Rule::Windows { windows } => {
                for w in windows {
                    let (start, end) = (time_of_day(&w.start), time_of_day(&w.end));
                    let length = if end <= start { end + DAY_MS - start } else { end - start };
                    out.push((midnight + start, length));
                }
            }
        }
    }
}

impl Schedule {
    /// The occurrences of `event` from a week before `now` (UTC unix ms) to a week after.
    fn around(&self, event: &TimedEvent, now: i64) -> impl Iterator<Item = Occurrence> {
        let shift = self.utc_offset_minutes * MINUTE_MS;
        let today = (now + shift).div_euclid(DAY_MS);
        let mut starts = Vec::new();
        for day in today - 7..=today + 7 {
            event.rule.on_day(day, &mut starts);
        }
        starts.into_iter().map(move |(start, length)| Occurrence { start: start - shift, end: start - shift + length })
    }

    /// The occurrence of `event` that is on at `now` (UTC unix ms), else the next one (within a week).
    pub fn current_or_next(&self, event: &TimedEvent, now: i64) -> Option<Occurrence> {
        self.around(event, now).filter(|o| o.is_on(now) || o.start > now).min_by_key(|o| (!o.is_on(now), o.start))
    }

    /// The last occurrence of `event` that started before `now` and is not on any more (within a week).
    pub fn previous(&self, event: &TimedEvent, now: i64) -> Option<Occurrence> {
        self.around(event, now).filter(|o| o.start <= now && !o.is_on(now)).max_by_key(|o| o.start)
    }

    /// Unix ms as the region's clock: (weekday 0 = Sunday, hour, minute, days from `now`'s local day).
    pub fn local_time(&self, at: i64, now: i64) -> (u32, u32, u32, i64) {
        let shift = self.utc_offset_minutes * MINUTE_MS;
        let local = at + shift;
        let day = local.div_euclid(DAY_MS);
        let in_day = local.rem_euclid(DAY_MS);
        ((day + 4).rem_euclid(7) as u32, (in_day / HOUR_MS) as u32, ((in_day % HOUR_MS) / MINUTE_MS) as u32, day - (now + shift).div_euclid(DAY_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Thursday 2026-10-22 18:30 in São Paulo (21:30 UTC)
    const NOW: i64 = 1_792_704_600_000;

    fn sa() -> &'static Schedule {
        let s = get();
        assert_eq!(s.utc_offset_minutes, -180);
        s
    }

    fn event(id: &str) -> &'static TimedEvent {
        sa().events.iter().find(|e| e.id == id).unwrap()
    }

    #[test]
    fn the_test_clock_is_thursday_evening_in_sao_paulo() {
        assert_eq!(sa().local_time(NOW, NOW), (4, 18, 30, 0));
    }

    #[test]
    fn daily_reset_is_four_in_the_morning() {
        let o = sa().current_or_next(event("daily_reset"), NOW).unwrap();
        assert_eq!(sa().local_time(o.start, NOW), (5, 4, 0, 1));
        assert_eq!(o.start % DAY_MS, 7 * HOUR_MS, "07:00 UTC");
    }

    #[test]
    fn weekly_reset_is_wednesday() {
        let o = sa().current_or_next(event("weekly_reset"), NOW).unwrap();
        assert_eq!(sa().local_time(o.start, NOW), (3, 4, 0, 6));
    }

    #[test]
    fn an_event_on_now_comes_before_the_next() {
        let o = sa().current_or_next(event("dimensional_invasion"), NOW).unwrap();
        assert!(o.is_on(NOW));
        assert_eq!(sa().local_time(o.start, NOW), (4, 18, 30, 0));
        let o = sa().current_or_next(event("shugo_festival"), NOW).unwrap();
        assert_eq!(sa().local_time(o.start, NOW), (4, 19, 0, 0));
    }

    #[test]
    fn the_previous_one_is_over() {
        let o = sa().previous(event("spacetime_rift"), NOW).unwrap();
        assert_eq!(sa().local_time(o.start, NOW), (4, 17, 0, 0));
        let o = sa().previous(event("weekly_reset"), NOW).unwrap();
        assert_eq!(sa().local_time(o.start, NOW), (3, 4, 0, -1));
    }

    #[test]
    fn rift_every_three_hours_from_two() {
        let o = sa().current_or_next(event("spacetime_rift"), NOW).unwrap();
        assert_eq!(sa().local_time(o.start, NOW), (4, 20, 0, 0));
    }

    #[test]
    fn a_window_is_on_until_its_end() {
        let rule = Rule::Windows { windows: vec![Window { start: "11:00".into(), end: "14:00".into() }] };
        let battlefield = TimedEvent { id: "x".into(), name: "x".into(), kind: "pvp".into(), note: String::new(), rule };
        let noon = NOW - 6 * HOUR_MS;
        let o = sa().current_or_next(&battlefield, noon).unwrap();
        assert!(o.is_on(noon));
        assert_eq!(o.end - noon, 90 * MINUTE_MS, "12:30, open until 14:00");
    }
}
