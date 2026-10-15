use serde::{Deserialize, Serialize};

use crate::catalog::zones;
use crate::combat::CombatLog;
use crate::entities::EntityDirectory;
use crate::segment::{Micros, SECOND};

pub const TRIVIAL_STAY: Micros = 120 * SECOND;
pub const RETENTION: Micros = 7 * 24 * 3600 * SECOND;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionRow {
    pub id: i32,
    pub name: String,
    pub class: Option<String>,
    pub damage: i64,
    pub dps: Option<f64>,
    pub heal: i64,
    pub hits: u32,
    pub crits: u32,
    pub local: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionSummary {
    pub zone_id: i32,
    pub zone_name: String,
    pub instance: bool,
    pub start: Micros,
    pub end: Micros,
    pub fight_seconds: f64,
    pub rows: Vec<SessionRow>,
}

impl SessionSummary {
    pub fn total(&self) -> i64 {
        self.rows.iter().map(|r| r.damage).sum()
    }
}

pub fn player_rows(combat: &CombatLog, entities: &EntityDirectory) -> Vec<SessionRow> {
    let local = entities.local_player_id();
    let healing = combat.healing_by_source();
    let mut ids: Vec<i32> = combat.damage_by_source().keys().chain(healing.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut rows: Vec<SessionRow> = ids
        .into_iter()
        .filter(|id| entities.is_known_player(*id))
        .map(|id| {
            let stats = combat.damage_by_source().get(&id).cloned().unwrap_or_default();
            SessionRow {
                id,
                name: entities.name_for(id).unwrap_or_else(|| format!("Player #{id}")),
                class: entities.class_of(id).map(str::to_string),
                damage: stats.sum,
                dps: stats.rate(),
                heal: healing.get(&id).copied().unwrap_or(0),
                hits: stats.hits,
                crits: stats.crits,
                local: Some(id) == local,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.damage.cmp(&a.damage).then(b.heal.cmp(&a.heal)));
    rows
}

pub fn summarize(zone_id: i32, start: Micros, end: Micros, combat: &CombatLog, entities: &EntityDirectory) -> Option<SessionSummary> {
    if combat.events().is_empty() && end - start < TRIVIAL_STAY {
        return None;
    }
    let rows = player_rows(combat, entities);
    if rows.is_empty() {
        return None;
    }
    let fight_seconds = match (combat.started_at, combat.last_event_at()) {
        (Some(a), Some(b)) => (b - a) as f64 / 1e6,
        _ => 0.0,
    };
    Some(SessionSummary {
        zone_id,
        zone_name: if zone_id > 0 { zones::name_of(zone_id, "en").map(str::to_string).unwrap_or_else(|| format!("zone {zone_id}")) } else { "unknown zone".into() },
        instance: zone_id > 0 && zones::is_instance(zone_id),
        start,
        end,
        fight_seconds,
        rows,
    })
}

pub fn prune(sessions: &mut Vec<SessionSummary>, now: Micros) {
    sessions.retain(|s| now - s.end <= RETENTION);
}
