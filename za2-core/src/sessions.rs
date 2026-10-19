use serde::{Deserialize, Serialize};

use crate::catalog::zones;
use crate::buffs::{BuffTimeline, BuffUptime};
use crate::combat::{CombatLog, SkillUsage};
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
    #[serde(default)]
    pub taken: i64,
    #[serde(default)]
    pub skills: Vec<SkillUsage>,
    #[serde(default)]
    pub heal_skills: Vec<SkillUsage>,
    #[serde(default)]
    pub taken_skills: Vec<SkillUsage>,
    /// Damage dealt per `SessionSummary::bucket_seconds`.
    #[serde(default)]
    pub series: Vec<i64>,
    /// When the player died (seconds from the start).
    #[serde(default)]
    pub deaths: Vec<f64>,
    /// The timed effects on the player.
    #[serde(default)]
    pub buffs: Vec<BuffUptime>,
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
    /// The boss of a saved boss fight; None for a whole stay in a zone.
    #[serde(default)]
    pub boss: Option<String>,
    #[serde(default = "default_bucket")]
    pub bucket_seconds: f64,
}

fn default_bucket() -> f64 {
    1.0
}

/// The time a chart point stands for: a second, or longer so a long log has at most `MAX_POINTS` points.
pub fn bucket_for(seconds: f64) -> Micros {
    const MAX_POINTS: f64 = 600.0;
    ((seconds / MAX_POINTS).ceil().max(1.0) * SECOND as f64) as Micros
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
        .filter(|id| entities.is_known_player(*id) || Some(*id) == local)
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
                taken: combat.taken_by_target().get(&id).copied().unwrap_or(0),
                skills: Vec::new(),
                heal_skills: Vec::new(),
                taken_skills: Vec::new(),
                series: Vec::new(),
                deaths: Vec::new(),
                buffs: Vec::new(),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.damage.cmp(&a.damage).then(b.heal.cmp(&a.heal)));
    rows
}

/// Effects kept per player in a saved session.
const SAVED_BUFFS: usize = 40;

pub fn summarize(zone_id: i32, start: Micros, end: Micros, combat: &CombatLog, entities: &EntityDirectory, buffs: &BuffTimeline) -> Option<SessionSummary> {
    if combat.events().is_empty() && end - start < TRIVIAL_STAY {
        return None;
    }
    let mut rows = player_rows(combat, entities);
    if rows.is_empty() {
        return None;
    }
    let fight_seconds = match (combat.started_at, combat.last_event_at()) {
        (Some(a), Some(b)) => (b - a) as f64 / 1e6,
        _ => 0.0,
    };
    let bucket = bucket_for(fight_seconds);
    let mut series = combat.series(false, bucket);
    for row in &mut rows {
        row.skills = combat.skill_breakdown(row.id, false);
        row.heal_skills = combat.skill_breakdown(row.id, true);
        row.taken_skills = combat.taken_breakdown(row.id);
        row.series = series.remove(&row.id).unwrap_or_default();
        if let (Some(from), Some(to)) = (combat.started_at, combat.last_event_at()) {
            row.deaths = combat.deaths().iter().filter(|d| d.1 == row.id).map(|d| (d.0 - from) as f64 / 1e6).collect();
            row.buffs = buffs.uptimes(row.id, from, to);
            row.buffs.truncate(SAVED_BUFFS);
        }
    }
    Some(SessionSummary {
        zone_id,
        zone_name: if zone_id > 0 { zones::name_of(zone_id, "en").map(str::to_string).unwrap_or_else(|| format!("zone {zone_id}")) } else { "unknown zone".into() },
        instance: zone_id > 0 && zones::is_instance(zone_id),
        start,
        end,
        fight_seconds,
        rows,
        boss: None,
        bucket_seconds: bucket as f64 / SECOND as f64,
    })
}

pub fn prune(sessions: &mut Vec<SessionSummary>, now: Micros) {
    sessions.retain(|s| now - s.end <= RETENTION);
}
