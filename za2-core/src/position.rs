use crate::catalog::maps::{self, MapInfo};
use crate::entities::{EntityDirectory, Position, PositionSource};
use crate::memory_stream::SelfFix;
use crate::segment::{Micros, SECOND};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionState {
    Exact,
    Estimated,
    Stale,
    Unknown,
}

impl PositionState {
    pub fn label(self) -> &'static str {
        match self {
            PositionState::Exact => "exact",
            PositionState::Estimated => "estimated",
            PositionState::Stale => "stale",
            PositionState::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackedPosition {
    pub map: Option<&'static MapInfo>,
    pub x: f32,
    pub y: f32,
    /// Height, known when the position comes from the game's memory.
    pub z: Option<f32>,
    pub raw: Option<Position>,
    pub state: PositionState,
    pub heading: f64,
    pub age: Micros,
}

const EXACT_FRESH: Micros = 6 * SECOND;
/// A memory fix older than this means the reader stopped; the packet estimate takes over.
pub const MEMORY_FRESH: Micros = 2 * SECOND;

#[derive(Default)]
pub struct PositionTracker {
    current: Option<&'static MapInfo>,
    steady: Option<(f32, f32)>,
    steady_at: Micros,
    heading_from: Option<(f32, f32)>,
    heading: f64,
    last_known: Option<(f32, f32)>,
    last_detect: Micros,
}

pub fn classify(position: Option<Position>, now: Micros, map_known: bool, stale: bool) -> PositionState {
    match position {
        None => PositionState::Unknown,
        Some(_) if !map_known || stale => PositionState::Stale,
        Some(p) => match p.source {
            PositionSource::Memory if now - p.at < MEMORY_FRESH => PositionState::Exact,
            PositionSource::Exact if now - p.at < EXACT_FRESH => PositionState::Exact,
            PositionSource::Exact | PositionSource::Memory => PositionState::Stale,
            PositionSource::Combat | PositionSource::Nearby => PositionState::Estimated,
        },
    }
}

impl PositionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn restore(&mut self, map_key: Option<&str>, last: Option<(f32, f32)>) {
        if self.current.is_none() {
            self.current = map_key.and_then(maps::by_key);
        }
        if self.last_known.is_none() {
            self.last_known = last;
        }
    }

    pub fn current_map(&self) -> Option<&'static MapInfo> {
        self.current
    }

    pub fn forget(&mut self) {
        self.steady = None;
        self.heading_from = None;
    }

    /// `memory` is the newest own position read from the game's memory and when it arrived. While it is fresh it
    /// wins outright: it is exact, and the engine's world name names the map.
    pub fn update(&mut self, directory: &EntityDirectory, now: Micros, memory: Option<(&SelfFix, Micros)>) -> TrackedPosition {
        if let Some((fix, at)) = memory.filter(|(_, at)| now - at < MEMORY_FRESH) {
            return self.from_memory(fix, at, now);
        }
        if now - self.last_detect >= SECOND {
            self.last_detect = now;
            if let Some(found) = maps::detect(&directory.recent_mob_positions()) {
                if Some(found) != self.current {
                    self.current = Some(found);
                    self.steady = None;
                }
            }
        }

        let position = directory.best_position(now);
        let stale = position.is_some_and(|p| position_is_stale(directory, p, now));
        let known = position.is_some() && self.current.is_some() && !stale;
        let mut state = classify(position, now, self.current.is_some(), stale);

        let (x, y) = if known {
            let p = position.unwrap();
            let (sx, sy) = self.steady(directory, p, now);
            self.last_known = Some((sx, sy));
            self.follow_heading(sx, sy);
            (sx, sy)
        } else {
            match self.last_known {
                Some(last) => {
                    if state == PositionState::Unknown {
                        state = PositionState::Stale;
                    }
                    last
                }
                None => {
                    state = PositionState::Unknown;
                    (0.0, 0.0)
                }
            }
        };

        TrackedPosition {
            map: self.current,
            x,
            y,
            raw: position,
            state,
            z: None,
            heading: self.heading,
            age: position.map_or(0, |p| now - p.at),
        }
    }

    fn from_memory(&mut self, fix: &SelfFix, at: Micros, now: Micros) -> TrackedPosition {
        if let Some(world) = fix.world.as_deref() {
            let map = maps::by_world(world);
            if map != self.current {
                self.current = map;
                self.steady = None;
            }
        }
        self.last_known = Some((fix.x, fix.y));
        self.steady = Some((fix.x, fix.y));
        self.steady_at = now;
        self.heading_from = Some((fix.x, fix.y));
        self.heading = fix.yaw as f64 + 90.0;
        let raw = Position { x: fix.x, y: fix.y, at, source: PositionSource::Memory };
        TrackedPosition {
            map: self.current,
            x: fix.x,
            y: fix.y,
            z: Some(fix.z),
            raw: Some(raw),
            state: classify(Some(raw), now, self.current.is_some(), false),
            heading: self.heading,
            age: now - at,
        }
    }

    fn steady(&mut self, directory: &EntityDirectory, p: Position, now: Micros) -> (f32, f32) {
        let dt = ((now - self.steady_at) as f64 / 1e6).clamp(0.0, 1.0);
        self.steady_at = now;
        let exact = directory.exact_position().is_some_and(|lp| now - lp.2 < EXACT_FRESH && lp.0 == p.x && lp.1 == p.y);
        let Some(cur) = self.steady else {
            self.steady = Some((p.x, p.y));
            return (p.x, p.y);
        };
        let (dx, dy) = ((p.x - cur.0) as f64, (p.y - cur.1) as f64);
        let dist = (dx * dx + dy * dy).sqrt();
        if exact || dist > 30000.0 {
            self.steady = Some((p.x, p.y));
            return (p.x, p.y);
        }
        let step = dist.min((dist * (1.0 - 0.94f64.powf(dt * 20.0))).min(1500.0 * dt));
        if dist > 0.0 {
            self.steady = Some(((cur.0 as f64 + dx / dist * step) as f32, (cur.1 as f64 + dy / dist * step) as f32));
        }
        self.steady.unwrap()
    }

    fn follow_heading(&mut self, x: f32, y: f32) {
        let Some(from) = self.heading_from else {
            self.heading_from = Some((x, y));
            return;
        };
        let (dx, dy) = ((x - from.0) as f64, (y - from.1) as f64);
        if dx * dx + dy * dy >= 800.0 * 800.0 {
            self.heading = dy.atan2(dx).to_degrees() + 90.0;
            self.heading_from = Some((x, y));
        }
    }
}

pub fn position_is_stale(directory: &EntityDirectory, position: Position, now: Micros) -> bool {
    let mobs = directory.recent_mob_positions();
    if mobs.len() < 5 || now - position.at < 30 * SECOND {
        return false;
    }
    let mut distances: Vec<f64> = mobs
        .iter()
        .map(|m| {
            let (dx, dy) = ((m.1 - position.x) as f64, (m.2 - position.y) as f64);
            (dx * dx + dy * dy).sqrt()
        })
        .collect();
    distances.sort_by(|a, b| a.total_cmp(b));
    distances[distances.len() / 2] > 60000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_fix_wins_and_names_the_map() {
        let mut d = EntityDirectory::new();
        let t = 100 * SECOND;
        d.note_local_position(10.0, 20.0, t);
        let mut tracker = PositionTracker::new();
        let fix = SelfFix { x: -60622.0, y: -73038.0, z: 27230.0, yaw: -45.0, world: Some("World_D_A".into()) };
        let p = tracker.update(&d, t + SECOND, Some((&fix, t + SECOND)));
        assert_eq!((p.x, p.y, p.z, p.state), (-60622.0, -73038.0, Some(27230.0), PositionState::Exact));
        assert_eq!(p.map.map(|m| m.key.as_str()), Some("altgard"));
        assert_eq!(p.heading, 45.0, "yaw 0 points along +x, which the map draws as 90 degrees");

        let late = t + SECOND + MEMORY_FRESH + SECOND;
        let p = tracker.update(&d, late, Some((&fix, t + SECOND)));
        assert_ne!(p.raw.map(|r| r.source), Some(PositionSource::Memory), "a stale memory fix gives way to the packets");
        assert_eq!(p.map.map(|m| m.key.as_str()), Some("altgard"), "the map stays");

        let elsewhere = SelfFix { world: Some("Dungeon_X".into()), ..fix };
        let p = tracker.update(&d, late, Some((&elsewhere, late)));
        assert_eq!((p.map, p.state), (None, PositionState::Stale), "a world without a map is not drawn on the last map");
    }
}
