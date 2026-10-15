use std::collections::{HashMap, HashSet, VecDeque};

use crate::catalog::bosses;
use crate::segment::{Micros, SECOND};

#[derive(Clone, Debug, PartialEq)]
pub struct CharacterInfo {
    pub combat_id: i32,
    pub name: String,
    pub class_code: i32,
    pub level: i32,
    pub server_id: u16,
    pub received_at: Micros,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HpSample {
    pub at: Micros,
    pub hp: i64,
}

#[derive(Default)]
struct HpTrack {
    highest_seen: i64,
    reported_max: i64,
    samples: Vec<HpSample>,
    resets: Vec<Micros>,
}

#[derive(Default)]
pub struct HitPoints {
    tracks: HashMap<i32, HpTrack>,
}

const RESET_FROM_BELOW: f64 = 0.95;
const RESET_RISE: f64 = 0.10;
const MAX_SAMPLES_PER_ENTITY: usize = 20_000;

impl HitPoints {
    pub fn note(&mut self, entity: i32, at: Micros, hp: i64) {
        let track = self.tracks.entry(entity).or_default();
        if let Some(last) = track.samples.last() {
            let previous = last.hp;
            let reset = (hp >= track.highest_seen && (previous as f64) < track.highest_seen as f64 * RESET_FROM_BELOW)
                || (hp - previous) as f64 >= track.highest_seen as f64 * RESET_RISE;
            if reset {
                track.resets.push(at);
            }
        }
        track.highest_seen = track.highest_seen.max(hp);
        if track.samples.len() >= MAX_SAMPLES_PER_ENTITY {
            track.samples.drain(..MAX_SAMPLES_PER_ENTITY / 2);
        }
        track.samples.push(HpSample { at, hp });
    }

    pub fn note_max(&mut self, entity: i32, max: i64) {
        if max > 0 {
            self.tracks.entry(entity).or_default().reported_max = max;
        }
    }

    pub fn highest_seen(&self, entity: i32) -> Option<i64> {
        self.tracks.get(&entity).map(|t| t.highest_seen)
    }

    pub fn max_of(&self, entity: i32) -> Option<i64> {
        let track = self.tracks.get(&entity)?;
        let latest = track.samples.last().map_or(0, |s| s.hp);
        if track.reported_max >= latest.max(1) {
            return Some(track.reported_max);
        }
        (track.highest_seen > 0).then_some(track.highest_seen)
    }

    pub fn latest(&self, entity: i32) -> Option<HpSample> {
        self.tracks.get(&entity).and_then(|t| t.samples.last().copied())
    }

    pub fn resets_of(&self, entity: i32) -> &[Micros] {
        self.tracks.get(&entity).map_or(&[], |t| t.resets.as_slice())
    }

    pub fn samples(&self, entity: i32) -> &[HpSample] {
        self.tracks.get(&entity).map_or(&[], |t| t.samples.as_slice())
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvObject {
    pub kind: String,
    pub key: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub at: Micros,
}

/// A monster or NPC the server reported in view, where it was last seen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveMob {
    pub entity: i32,
    pub npc: i32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PositionSource {
    /// Read from the game's memory by `za2-memory`.
    Memory,
    Exact,
    /// The monster the local player is fighting.
    Combat,
    /// Median of the entities the server reports around the player.
    Nearby,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub at: Micros,
    pub source: PositionSource,
}

const PARTY_MEMORY: Micros = 90 * SECOND;
const MIN_SKILLS_OF_A_PLAYER: usize = 4;

/// Gear score and combat power of a party member, from the party roster frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartyStats {
    pub gear_score: u32,
    pub combat_power: Option<u64>,
}

#[derive(Default)]
pub struct EntityDirectory {
    names: HashMap<i32, String>,
    ids: HashMap<String, i32>,
    class_votes: HashMap<i32, Vec<(&'static str, i32)>>,
    roster: HashMap<String, i32>,
    not_players: HashSet<String>,
    guilds: HashMap<i32, String>,
    explicit_local_id: Option<i32>,
    character: Option<CharacterInfo>,
    configured_local_name: Option<String>,

    position: Option<(f32, f32, Micros)>,
    mob_positions: VecDeque<(i32, f32, f32)>,
    live_mobs: HashMap<i32, LiveMob>,
    /// Last reported place of every entity in view (spawns and movement frames).
    nearby: HashMap<i32, (f32, f32, Micros)>,
    local_attacker: Option<(i32, Micros)>,
    env: HashMap<i32, EnvObject>,

    npc_ids: HashMap<i32, i32>,
    boss_npcs: HashMap<i32, i32>,
    spawned: HashSet<i32>,
    removed: HashMap<i32, (Micros, u8)>,
    summon_owners: HashMap<i32, i32>,
    summon_owner_names: HashMap<i32, String>,
    resolved_owners: Vec<(i32, i32)>,

    party_seen: HashMap<String, Micros>,
    last_party_frame: Micros,
    party_classes: HashMap<String, &'static str>,
    party_stats: HashMap<String, PartyStats>,
    party_targets: HashSet<i32>,
    fighting_alongside: HashSet<i32>,
    skills_seen: HashMap<i32, HashSet<i32>>,
    detailed_stats: HashMap<i32, f64>,
    faction_by_id: HashMap<i32, u8>,
    local_hit: Option<(i32, Micros)>,
    local_id_changes: Vec<(i32, i32)>,

    pub hit_points: HitPoints,
}

/// An own position from a position or effect frame wins outright for this long.
const EXACT_HOLD: Micros = 6 * SECOND;
/// ... and over the median of the surroundings for this long.
const NEARBY_HOLD: Micros = 15 * SECOND;
/// A hit given or taken this recently means the player stands at the monster.
const COMBAT_WINDOW: Micros = 3 * SECOND;
const COMBAT_PLACE_AGE: Micros = 10 * SECOND;
/// Entities that moved or appeared this recently count as being around the player.
const NEARBY_WINDOW: Micros = 20 * SECOND;

fn majority(votes: &Vec<(&'static str, i32)>) -> Option<&'static str> {
    let mut best: Option<(&'static str, i32)> = None;
    for &(class, count) in votes {
        if best.map_or(true, |(_, c)| count > c) {
            best = Some((class, count));
        }
    }
    best.map(|b| b.0)
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

impl EntityDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_configured_local_name(&mut self, name: Option<&str>) {
        self.configured_local_name = name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    }

    pub fn local_player_id(&self) -> Option<i32> {
        if let Some(id) = self.explicit_local_id {
            return Some(id);
        }
        if let Some(id) = self.configured_local_name.as_ref().and_then(|n| self.ids.get(n)) {
            return Some(*id);
        }
        self.infer_local_player()
    }

    pub fn set_explicit_local_id(&mut self, id: i32) {
        self.explicit_local_id = Some(id);
    }

    pub fn is_local_player(&self, id: i32) -> bool {
        self.local_player_id() == Some(id)
    }

    pub fn local_character(&self) -> Option<&CharacterInfo> {
        self.character.as_ref()
    }

    pub fn learned_local_name(&self) -> Option<&str> {
        self.character.as_ref().map(|c| c.name.as_str()).filter(|n| !n.is_empty())
    }

    pub fn set_local_character(&mut self, info: CharacterInfo) {
        let previous_id = self.explicit_local_id;
        let previous_name = self.character.as_ref().map(|c| c.name.clone());
        self.explicit_local_id = Some(info.combat_id);
        self.names.insert(info.combat_id, info.name.clone());
        self.ids.insert(info.name.clone(), info.combat_id);
        let renamed = matches!(previous_id, Some(old) if old != info.combat_id) && previous_name.as_deref() == Some(info.name.as_str());
        if renamed {
            self.forget_place();
            self.local_id_changes.push((previous_id.unwrap(), info.combat_id));
        }
        self.character = Some(info);
    }

    pub fn drain_local_id_changes(&mut self) -> Vec<(i32, i32)> {
        std::mem::take(&mut self.local_id_changes)
    }

    pub fn exact_position(&self) -> Option<(f32, f32, Micros)> {
        self.position
    }

    pub fn best_position(&self, now: Micros) -> Option<Position> {
        let exact_age = self.position.map(|p| now - p.2);
        if exact_age.is_some_and(|age| age <= EXACT_HOLD) {
            return self.position.map(|(x, y, at)| Position { x, y, at, source: PositionSource::Exact });
        }

        let fighting = [self.local_hit, self.local_attacker]
            .into_iter()
            .flatten()
            .filter(|(_, at)| now - at <= COMBAT_WINDOW)
            .filter_map(|(id, _)| self.nearby.get(&id).filter(|n| now - n.2 <= COMBAT_PLACE_AGE))
            .max_by_key(|n| n.2);
        if let Some(&(x, y, at)) = fighting {
            return Some(Position { x, y, at, source: PositionSource::Combat });
        }

        if exact_age.map_or(true, |age| age > NEARBY_HOLD) {
            let around: Vec<&(f32, f32, Micros)> = self.nearby.values().filter(|n| now - n.2 <= NEARBY_WINDOW).collect();
            if around.len() >= 2 {
                let mut xs: Vec<f32> = around.iter().map(|n| n.0).collect();
                let mut ys: Vec<f32> = around.iter().map(|n| n.1).collect();
                let newest = around.iter().map(|n| n.2).max().unwrap();
                return Some(Position { x: median(&mut xs), y: median(&mut ys), at: newest, source: PositionSource::Nearby });
            }
        }

        self.position.map(|(x, y, at)| Position { x, y, at, source: PositionSource::Exact })
    }

    /// A movement frame of another entity: where it is now.
    pub fn note_movement(&mut self, entity: i32, x: f32, y: f32, z: f32, at: Micros) {
        if let Some(mob) = self.live_mobs.get_mut(&entity) {
            (mob.x, mob.y, mob.z) = (x, y, z);
        }
        self.note_nearby(entity, x, y, at);
    }

    fn note_nearby(&mut self, entity: i32, x: f32, y: f32, at: Micros) {
        self.nearby.insert(entity, (x, y, at));
        if self.nearby.len() > 2000 {
            self.nearby.retain(|_, n| at - n.2 <= NEARBY_WINDOW);
        }
    }

    pub fn note_attacked_local(&mut self, attacker: i32, at: Micros) {
        self.local_attacker = Some((attacker, at));
    }

    pub fn note_local_position(&mut self, x: f32, y: f32, at: Micros) {
        self.position = Some((x, y, at));
    }

    pub fn note_mob_position(&mut self, entity: i32, npc: i32, x: f32, y: f32, z: f32, at: Micros) {
        self.live_mobs.insert(entity, LiveMob { entity, npc, x, y, z });
        self.note_nearby(entity, x, y, at);
        if self.live_mobs.len() > 1000 {
            self.live_mobs.clear();
        }
        self.mob_positions.push_back((npc, x, y));
        while self.mob_positions.len() > 60 {
            self.mob_positions.pop_front();
        }
    }

    pub fn forget_live_mob(&mut self, entity: i32) {
        self.live_mobs.remove(&entity);
        self.nearby.remove(&entity);
    }

    pub fn live_mobs(&self) -> impl Iterator<Item = &LiveMob> {
        self.live_mobs.values()
    }

    pub fn recent_mob_positions(&self) -> Vec<(i32, f32, f32)> {
        self.mob_positions.iter().copied().collect()
    }

    pub fn note_env(&mut self, entity: i32, kind: &str, key: &str, x: f32, y: f32, z: f32, at: Micros) {
        self.env.insert(entity, EnvObject { kind: kind.to_string(), key: key.to_string(), x, y, z, at });
        if self.env.len() > 3000 {
            self.env.clear();
        }
    }

    pub fn forget_env(&mut self, entity: i32) {
        self.env.remove(&entity);
    }

    pub fn live_env(&mut self, now: Micros) -> Vec<EnvObject> {
        self.env.retain(|_, e| now - e.at <= 15 * 60 * SECOND);
        self.env.values().cloned().collect()
    }

    pub fn forget_place(&mut self) {
        self.position = None;
        self.env.clear();
        self.mob_positions.clear();
        self.live_mobs.clear();
        self.nearby.clear();
        self.local_attacker = None;
    }

    pub fn note_local_hit(&mut self, entity: i32, at: Micros) {
        self.local_hit = Some((entity, at));
    }

    pub fn local_hit_target(&self) -> Option<(i32, Micros)> {
        self.local_hit
    }

    pub fn clear_local_target(&mut self, entity: i32) {
        if self.local_hit.is_some_and(|h| h.0 == entity) {
            self.local_hit = None;
        }
    }

    pub fn npc_id_of(&self, entity: i32) -> Option<i32> {
        self.npc_ids.get(&entity).copied()
    }

    pub fn register_npc(&mut self, entity: i32, npc: i32) {
        self.npc_ids.insert(entity, npc);
        if bosses::find(npc).is_some() {
            self.boss_npcs.insert(entity, npc);
        }
    }

    pub fn boss_npc_id_of(&self, entity: i32) -> Option<i32> {
        self.boss_npcs.get(&entity).copied()
    }

    pub fn note_spawned(&mut self, entity: i32) {
        self.spawned.insert(entity);
        self.removed.remove(&entity);
    }

    pub fn note_removed(&mut self, entity: i32, reason: u8, at: Micros) {
        self.removed.insert(entity, (at, reason));
        self.live_mobs.remove(&entity);
        self.nearby.remove(&entity);
    }

    pub fn is_spawned(&self, entity: i32) -> bool {
        self.spawned.contains(&entity)
    }

    pub fn is_known_monster(&self, entity: i32) -> bool {
        self.spawned.contains(&entity) && !self.summon_owners.contains_key(&entity) && !self.summon_owner_names.contains_key(&entity)
    }

    pub fn drain_resolved_owners(&mut self) -> Vec<(i32, i32)> {
        std::mem::take(&mut self.resolved_owners)
    }

    pub fn set_summon_owner(&mut self, entity: i32, owner: Option<i32>) {
        match owner {
            Some(owner) => {
                if self.summon_owners.get(&entity) != Some(&owner) && self.resolved_owners.len() < 4096 {
                    self.resolved_owners.push((entity, owner));
                }
                self.summon_owners.insert(entity, owner);
            }
            None => {
                self.summon_owners.remove(&entity);
            }
        }
    }

    pub fn set_summon_owner_name(&mut self, entity: i32, owner_name: Option<String>) {
        match owner_name {
            Some(name) => {
                self.summon_owner_names.insert(entity, name);
            }
            None => {
                self.summon_owner_names.remove(&entity);
            }
        }
    }

    pub fn summon_owner_of(&self, entity: i32) -> Option<i32> {
        if let Some(owner) = self.summon_owners.get(&entity) {
            return Some(*owner);
        }
        let name = self.summon_owner_names.get(&entity)?;
        self.ids.get(name).copied().filter(|&id| id != entity)
    }

    pub fn note_faction(&mut self, id: i32, faction: u8) {
        self.faction_by_id.insert(id, faction);
    }

    pub fn faction_of(&self, id: i32) -> Option<&'static str> {
        match self.faction_by_id.get(&id) {
            Some(1) => Some("Elyos"),
            Some(2) => Some("Asmodian"),
            _ => None,
        }
    }

    pub fn set_guild(&mut self, id: i32, guild: String) {
        self.guilds.insert(id, guild);
    }

    pub fn guild_of(&self, id: i32) -> Option<&str> {
        self.guilds.get(&id).map(String::as_str)
    }

    pub fn note_roster_name(&mut self, name: &str) {
        *self.roster.entry(name.to_string()).or_default() += 1;
    }

    pub fn note_party(&mut self, names: &[String], at: Micros) {
        for name in names {
            self.party_seen.insert(name.clone(), at);
        }
        self.last_party_frame = at;
        self.match_party_members_by_class();
    }

    pub fn note_party_class(&mut self, name: &str, class: &'static str) {
        self.party_classes.insert(name.to_string(), class);
    }

    pub fn note_party_stats(&mut self, name: &str, stats: PartyStats) {
        self.party_stats.insert(name.to_string(), stats);
    }

    /// Gear score and combat power of a party member, from the last roster that named them.
    pub fn party_stats(&self, name: &str) -> Option<PartyStats> {
        self.party_stats.get(name).copied()
    }

    pub fn note_non_player_name(&mut self, name: &str) {
        self.not_players.insert(name.to_string());
    }

    pub fn party_names(&self) -> HashSet<String> {
        self.party_seen.iter().filter(|(_, at)| self.last_party_frame - **at <= PARTY_MEMORY).map(|(n, _)| n.clone()).collect()
    }

    pub fn has_name(&self, id: i32) -> bool {
        self.names.contains_key(&id)
    }

    pub fn party_member_ids_of_class(&self, class: &str) -> Vec<i32> {
        let mut ids: Vec<i32> = Vec::new();
        for name in self.party_names() {
            if let Some(&id) = self.ids.get(&name) {
                if !ids.contains(&id) && self.class_votes.get(&id).and_then(majority) == Some(class) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    pub fn party_player_ids_of_class(&self, class: &str) -> Vec<i32> {
        let mut ids = self.party_member_ids_of_class(class);
        if let Some(local) = self.infer_local_player() {
            if !ids.contains(&local) && self.class_of(local) == Some(class) {
                ids.push(local);
            }
        }
        ids
    }

    pub fn is_named_outside_party(&self, id: i32) -> bool {
        let Some(name) = self.names.get(&id) else { return false };
        let party = self.party_names();
        !party.is_empty() && !party.contains(name)
    }

    pub fn note_monster_hit(&mut self, player: i32, monster: i32, skill_id: i32) {
        let party_side = match self.names.get(&player) {
            Some(name) => self.party_names().contains(name),
            None => self.is_local_player(player) || self.infer_local_player() == Some(player),
        };
        if party_side {
            if self.party_targets.len() > 4096 {
                self.party_targets.clear();
            }
            self.party_targets.insert(monster);
        } else if !self.names.contains_key(&player) && !self.spawned.contains(&player) && self.party_targets.contains(&monster) {
            self.fighting_alongside.insert(player);
            let skills = self.skills_seen.entry(player).or_default();
            if skills.insert(skill_id / 10000) && skills.len() == MIN_SKILLS_OF_A_PLAYER {
                self.match_party_members_by_class();
            }
        }
    }

    fn match_party_members_by_class(&mut self) {
        let party = self.party_names();
        let local = self.explicit_local_id.or_else(|| self.infer_local_player());
        let local_name = self.character.as_ref().map(|c| c.name.clone()).or_else(|| self.configured_local_name.clone());
        let mut by_class: HashMap<&'static str, Vec<String>> = HashMap::new();
        for name in &party {
            if self.ids.contains_key(name) || Some(name) == local_name.as_ref() {
                continue;
            }
            if let Some(&class) = self.party_classes.get(name) {
                by_class.entry(class).or_default().push(name.clone());
            }
        }

        for (class, members) in by_class {
            if members.len() != 1 {
                continue;
            }
            let mut candidates: Vec<(i32, i32)> = self
                .fighting_alongside
                .iter()
                .copied()
                .filter(|id| {
                    !self.names.contains_key(id)
                        && Some(*id) != local
                        && !self.spawned.contains(id)
                        && self.skills_seen.get(id).is_some_and(|s| s.len() >= MIN_SKILLS_OF_A_PLAYER)
                        && self.class_votes.get(id).and_then(majority) == Some(class)
                })
                .map(|id| (id, self.class_votes[&id].iter().map(|v| v.1).sum()))
                .collect();
            candidates.sort_by(|a, b| b.1.cmp(&a.1));
            if candidates.len() == 1 || (candidates.len() > 1 && candidates[0].1 >= 3 * candidates[1].1) {
                let id = candidates[0].0;
                self.register(id, &members[0]);
                self.fighting_alongside.remove(&id);
            }
        }
    }

    pub fn note_detailed_stats(&mut self, entity: i32) {
        for value in self.detailed_stats.values_mut() {
            *value *= 0.95;
        }
        *self.detailed_stats.entry(entity).or_default() += 1.0;
        if self.detailed_stats.len() > 4096 {
            self.detailed_stats.retain(|_, v| *v > 0.01);
        }
    }

    pub fn infer_local_player(&self) -> Option<i32> {
        let (mut best_stat, mut second_stat, mut best_stat_id) = (f64::NEG_INFINITY, f64::NEG_INFINITY, 0);
        for (&id, &value) in &self.detailed_stats {
            if value > best_stat {
                second_stat = best_stat;
                best_stat = value;
                best_stat_id = id;
            } else if value > second_stat {
                second_stat = value;
            }
        }
        if !self.detailed_stats.is_empty() && best_stat >= 5.0 && (self.detailed_stats.len() == 1 || best_stat >= 3.0 * second_stat) {
            return Some(best_stat_id);
        }

        let (mut unnamed, mut best_id, mut best_votes, mut second_votes) = (0, 0, i32::MIN, i32::MIN);
        for (&id, votes) in &self.class_votes {
            if self.names.contains_key(&id) {
                continue;
            }
            let total: i32 = votes.iter().map(|v| v.1).sum();
            unnamed += 1;
            if total > best_votes {
                second_votes = best_votes;
                best_votes = total;
                best_id = id;
            } else if total > second_votes {
                second_votes = total;
            }
        }
        if unnamed == 0 || (unnamed > 1 && (best_votes as i64) < 3 * second_votes as i64) {
            return None;
        }
        Some(best_id)
    }

    fn local_roster_name(&self) -> Option<String> {
        let mut left: Vec<(&String, i32)> =
            self.roster.iter().filter(|(n, _)| !self.ids.contains_key(*n) && !self.not_players.contains(*n)).map(|(n, c)| (n, *c)).collect();
        left.sort_by(|a, b| b.1.cmp(&a.1));
        if left.is_empty() || (left.len() > 1 && left[0].1 < 3 * left[1].1) {
            return None;
        }
        Some(left[0].0.clone())
    }

    fn local_player_known_elsewhere(&self, id: i32) -> bool {
        if let Some(explicit) = self.explicit_local_id {
            return explicit != id;
        }
        match &self.configured_local_name {
            Some(own) => self.ids.get(own).is_some_and(|&owner| owner != id),
            None => false,
        }
    }

    pub fn name_for(&self, id: i32) -> Option<String> {
        let mut registered = self.names.get(&id).cloned();
        if registered.is_none() {
            if let Some(boss) = self.boss_npcs.get(&id).and_then(|npc| bosses::find(*npc)) {
                return Some(boss.name.clone());
            }
        }
        if registered.is_none() && self.infer_local_player() == Some(id) && !self.local_player_known_elsewhere(id) {
            registered = self.configured_local_name.clone().or_else(|| self.local_roster_name());
        }
        registered.or_else(|| self.class_of(id).map(|_| format!("Player #{id}")))
    }

    pub fn class_of(&self, id: i32) -> Option<&'static str> {
        self.class_votes.get(&id).and_then(majority)
    }

    pub fn note_class(&mut self, id: i32, class: &'static str) {
        let votes = self.class_votes.entry(id).or_default();
        match votes.iter_mut().find(|v| v.0 == class) {
            Some(v) => v.1 += 1,
            None => votes.push((class, 1)),
        }
    }

    pub fn is_known_player(&self, id: i32) -> bool {
        self.class_votes.contains_key(&id)
    }

    pub fn register(&mut self, id: i32, name: &str) {
        if let Some(before) = self.names.get(&id).cloned() {
            if before != name {
                self.guilds.remove(&id);
                if self.ids.get(&before) == Some(&id) {
                    self.ids.remove(&before);
                }
            }
        }
        self.names.insert(id, name.to_string());
        self.ids.insert(name.to_string(), id);

        let pending: Vec<i32> = self
            .summon_owner_names
            .iter()
            .filter(|(summon, owner)| owner.as_str() == name && **summon != id && !self.summon_owners.contains_key(summon))
            .map(|(summon, _)| *summon)
            .collect();
        for summon in pending {
            if self.resolved_owners.len() < 4096 {
                self.summon_owners.insert(summon, id);
                self.resolved_owners.push((summon, id));
            }
        }
    }

    pub fn monster_name_for(&self, entity: i32, language: &str) -> Option<&'static str> {
        self.npc_ids.get(&entity).and_then(|npc| crate::catalog::npcs::name_of(*npc, language))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_prefers_exact_then_combat_then_surroundings() {
        let mut d = EntityDirectory::new();
        let t = 100 * SECOND;
        d.note_movement(1, 1000.0, 1000.0, 0.0, t);
        d.note_movement(2, 3000.0, 1000.0, 0.0, t);
        d.note_movement(3, 2000.0, 5000.0, 0.0, t);
        let p = d.best_position(t).unwrap();
        assert_eq!((p.source, p.x, p.y), (PositionSource::Nearby, 2000.0, 1000.0));

        d.note_movement(2, 3500.0, 1200.0, 0.0, t + SECOND);
        d.note_local_hit(2, t + SECOND);
        let p = d.best_position(t + 2 * SECOND).unwrap();
        assert_eq!((p.source, p.x, p.y), (PositionSource::Combat, 3500.0, 1200.0));

        d.note_local_position(10.0, 20.0, t + 2 * SECOND);
        assert_eq!(d.best_position(t + 3 * SECOND).unwrap().source, PositionSource::Exact);

        let later = t + 2 * SECOND + EXACT_HOLD + SECOND;
        assert_eq!(d.best_position(later).unwrap().source, PositionSource::Exact, "exact holds over the surroundings");
        let much_later = t + 2 * SECOND + NEARBY_HOLD + SECOND;
        d.note_movement(1, 1000.0, 1000.0, 0.0, much_later);
        d.note_movement(3, 2000.0, 5000.0, 0.0, much_later);
        assert_eq!(d.best_position(much_later).unwrap().source, PositionSource::Nearby);
    }

    #[test]
    fn removed_and_quiet_entities_leave_the_estimate() {
        let mut d = EntityDirectory::new();
        let t = 100 * SECOND;
        d.note_movement(1, 1000.0, 1000.0, 0.0, t);
        d.note_movement(2, 3000.0, 1000.0, 0.0, t);
        d.note_movement(3, 2000.0, 1000.0, 0.0, t - NEARBY_WINDOW - SECOND);
        d.note_removed(2, 1, t);
        assert!(d.best_position(t).is_none());
    }

    #[test]
    fn movement_moves_live_monster_markers() {
        let mut d = EntityDirectory::new();
        d.note_mob_position(7, 1234, 100.0, 200.0, 50.0, SECOND);
        d.note_movement(7, 150.0, 260.0, 70.0, 2 * SECOND);
        assert_eq!(d.live_mobs().copied().collect::<Vec<_>>(), vec![LiveMob { entity: 7, npc: 1234, x: 150.0, y: 260.0, z: 70.0 }]);
    }
}
