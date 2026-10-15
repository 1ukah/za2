use std::collections::HashMap;

use crate::catalog::skills;
use crate::decoder::{DamageEvent, HitKinds};
use crate::segment::Micros;

const PERSONAL_SHIELDS: [&str; 2] = ["Defiance", "Warding Shield"];
const ABSORB_SHIELDS: [&str; 2] = ["Protection Circle", "Impeding Authority"];

fn english_name(e: &DamageEvent) -> &str {
    if e.skill_id != 0 {
        if let Some(name) = skills::english_name(e.skill_id) {
            return name;
        }
    }
    &e.skill
}

pub fn is_shield(e: &DamageEvent) -> bool {
    e.is_heal && {
        let name = english_name(e);
        PERSONAL_SHIELDS.contains(&name) || ABSORB_SHIELDS.contains(&name)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceStats {
    pub sum: i64,
    pub estimated_sum: i64,
    pub first: Micros,
    pub last: Micros,
    pub hits: u32,
    pub crits: u32,
}

impl SourceStats {
    fn add(&mut self, e: &DamageEvent) {
        if self.sum == 0 && self.hits == 0 {
            self.first = e.timestamp;
            self.last = e.timestamp;
        }
        self.sum += e.amount;
        if e.estimated {
            self.estimated_sum += e.amount;
        }
        self.first = self.first.min(e.timestamp);
        self.last = self.last.max(e.timestamp);
        if !e.is_tick {
            self.hits += 1;
            if e.is_critical {
                self.crits += 1;
            }
        }
    }

    pub fn rate(&self) -> Option<f64> {
        let seconds = (self.last - self.first) as f64 / 1e6;
        (seconds > 0.0).then(|| self.sum as f64 / seconds)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillUsage {
    pub skill: String,
    pub skill_id: i32,
    pub hits: u32,
    pub crit_hits: u32,
    pub total: i64,
    pub min: i64,
    pub max: i64,
    pub back: u32,
    pub perfect: u32,
    pub known_kinds: u32,
}

#[derive(Default)]
pub struct CombatLog {
    events: Vec<DamageEvent>,
    shields: Vec<DamageEvent>,
    damage: HashMap<i32, SourceStats>,
    healing: HashMap<i32, i64>,
    taken: HashMap<i32, i64>,
    pub started_at: Option<Micros>,
    pub version: u64,
}

impl CombatLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, events: &[DamageEvent]) {
        for e in events {
            if is_shield(e) {
                self.shields.push(e.clone());
            } else if e.is_heal || e.source != e.target {
                self.index(e);
                self.events.push(e.clone());
            }
        }
        if !events.is_empty() {
            self.version += 1;
        }
    }

    fn index(&mut self, e: &DamageEvent) {
        self.started_at.get_or_insert(e.timestamp);
        if e.is_heal {
            *self.healing.entry(e.source).or_default() += e.amount;
        } else {
            self.damage.entry(e.source).or_default().add(e);
            *self.taken.entry(e.target).or_default() += e.amount;
        }
    }

    fn rebuild(&mut self) {
        self.damage.clear();
        self.healing.clear();
        self.taken.clear();
        self.started_at = None;
        let events = std::mem::take(&mut self.events);
        for e in &events {
            self.index(e);
        }
        self.events = events;
        self.version += 1;
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.shields.clear();
        self.damage.clear();
        self.healing.clear();
        self.taken.clear();
        self.started_at = None;
        self.version += 1;
    }

    pub fn reattribute(&mut self, summon: i32, owner: i32) -> usize {
        let mut changed = 0;
        for e in &mut self.events {
            if e.source == summon && e.skill_id != 0 && (skills::class_of(e.skill_id).is_some() || skills::is_summon_attack(e.skill_id)) {
                e.source = owner;
                changed += 1;
            }
        }
        if changed > 0 {
            self.rebuild();
        }
        changed
    }

    pub fn merge_ids(&mut self, from: i32, to: i32) -> usize {
        if from == to {
            return 0;
        }
        let mut changed = 0;
        for e in self.events.iter_mut().chain(self.shields.iter_mut()) {
            if e.source == from || e.target == from {
                if e.source == from {
                    e.source = to;
                }
                if e.target == from {
                    e.target = to;
                }
                changed += 1;
            }
        }
        if changed > 0 {
            self.rebuild();
        }
        changed
    }

    pub fn events(&self) -> &[DamageEvent] {
        &self.events
    }

    pub fn damage_by_source(&self) -> &HashMap<i32, SourceStats> {
        &self.damage
    }

    pub fn healing_by_source(&self) -> &HashMap<i32, i64> {
        &self.healing
    }

    pub fn taken_by_target(&self) -> &HashMap<i32, i64> {
        &self.taken
    }

    pub fn last_event_at(&self) -> Option<Micros> {
        self.events.last().map(|e| e.timestamp)
    }

    pub fn skill_breakdown(&self, source: i32, heals: bool) -> Vec<SkillUsage> {
        skill_breakdown(self.events.iter().filter(|e| e.source == source), heals)
    }
}

pub fn skill_breakdown<'a>(events: impl Iterator<Item = &'a DamageEvent>, heals: bool) -> Vec<SkillUsage> {
    let mut groups: Vec<(String, Vec<&DamageEvent>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for e in events.filter(|e| e.is_heal == heals) {
        let key = if e.skill.is_empty() { "(auto attack)".to_string() } else { e.skill.clone() };
        let slot = *index.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[slot].1.push(e);
    }

    let mut result: Vec<SkillUsage> = groups
        .into_iter()
        .map(|(skill, all)| {
            let hits: Vec<&&DamageEvent> = all.iter().filter(|e| !e.is_tick).collect();
            let amounts: Vec<i64> = if hits.is_empty() { all.iter().map(|e| e.amount).collect() } else { hits.iter().map(|e| e.amount).collect() };
            let mut back = 0;
            let mut perfect = 0;
            let mut known = 0;
            for e in &hits {
                if let Some(HitKinds(k)) = e.kinds {
                    known += 1;
                    if k & HitKinds::BACK != 0 {
                        back += 1;
                    }
                    if k & HitKinds::PERFECT != 0 {
                        perfect += 1;
                    }
                }
            }
            SkillUsage {
                skill,
                skill_id: all.iter().map(|e| e.skill_id).find(|id| *id != 0).unwrap_or(0),
                hits: amounts.len() as u32,
                crit_hits: hits.iter().filter(|e| e.is_critical).count() as u32,
                total: all.iter().map(|e| e.amount).sum(),
                min: amounts.iter().copied().min().unwrap_or(0),
                max: amounts.iter().copied().max().unwrap_or(0),
                back,
                perfect,
                known_kinds: known,
            }
        })
        .collect();
    result.sort_by(|a, b| b.total.cmp(&a.total));
    result
}
