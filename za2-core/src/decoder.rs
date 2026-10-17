use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::catalog::{bosses, gather, skills, zones};
use crate::entities::{CharacterInfo, EntityDirectory, PartyStats};
use crate::lz4;
use crate::protocol::{OpcodeFamily, Protocol};
use crate::segment::{Micros, SECOND};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HitKinds(pub u16);

impl HitKinds {
    pub const PARRY: u16 = 1;
    pub const PERFECT: u16 = 2;
    pub const DOUBLE: u16 = 4;
    pub const BACK: u16 = 8;
    pub const FRONT: u16 = 16;
    pub const BLOCK: u16 = 32;
    pub const PERFECT_BLOCK: u16 = 64;
    pub const IRON_WALL: u16 = 128;
    pub const REGENERATION: u16 = 256;
}

#[derive(Clone, Debug, PartialEq)]
pub struct DamageEvent {
    pub timestamp: Micros,
    pub source: i32,
    pub target: i32,
    pub amount: i64,
    pub is_heal: bool,
    pub skill: String,
    pub is_critical: bool,
    pub is_tick: bool,
    pub skill_id: i32,
    pub kinds: Option<HitKinds>,
    pub hit_count: i32,
    pub estimated: bool,
}

impl DamageEvent {
    fn new(timestamp: Micros, source: i32, target: i32, amount: i64, is_heal: bool, skill: String) -> Self {
        DamageEvent {
            timestamp,
            source,
            target,
            amount,
            is_heal,
            skill,
            is_critical: false,
            is_tick: false,
            skill_id: 0,
            kinds: None,
            hit_count: 1,
            estimated: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneChange {
    pub timestamp: Micros,
    pub zone_id: i32,
    pub restart: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AvoidEvent {
    pub timestamp: Micros,
    pub source: i32,
    pub target: i32,
}

/// What a buff record says: an effect (abnormal) put on, changed or taken off an entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BuffChange {
    On,
    /// `restarted`: the change re-applied the effect, which starts its timer over.
    Changed { restarted: bool },
    /// Reason 1: ran out; 11: taken off by a skill; 5: a passive replaced by its next level.
    Off { reason: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuffEvent {
    pub timestamp: Micros,
    pub target: i32,
    /// Numbered per target; every stack is an instance of its own.
    pub instance: i64,
    /// The effect (0 for `Off`, which names only the instance).
    pub abnormal: u32,
    pub caster: i32,
    pub skill: Option<u32>,
    /// Duration (ms; -1 endless) and end (server unix ms).
    pub length_ms: i64,
    pub end_ms: i64,
    pub change: BuffChange,
}

/// An entity left the world: died (`died`) or despawned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Departure {
    pub timestamp: Micros,
    pub entity: i32,
    pub died: bool,
}

/// One field boss of a map report (sent when the player opens the map): up since `time_ms`, or back at `time_ms`
/// (server unix ms; 0: the game gives no time).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldBossSighting {
    pub boss: u32,
    pub up: bool,
    pub time_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldBossReport {
    pub timestamp: Micros,
    pub map_id: u32,
    pub bosses: Vec<FieldBossSighting>,
}

#[derive(Default, Clone, Debug)]
pub struct DecoderStats {
    pub skipped_short_frames: u64,
    pub unknown_opcodes: u64,
    pub no_damage_frames: u64,
    pub bundles: u64,
    pub bundle_failures: u64,
    pub decoded_frames: u64,
    pub opcode_counts: HashMap<u32, u64>,
}

const ZONE_OPCODE: u32 = 0x2136;
const HIT_NOTICE_OPCODE: u32 = 0x0338;
const BUFF_ON_OPCODE: u32 = 0x2a38;
const BUFF_RENEWED_OPCODE: u32 = 0x2b38;
const BUFF_OFF_OPCODE: u32 = 0x2c38;
const BOSS_MAP_OPCODE: u32 = 0x0191;
/// Counters of the character (all of them at login, the changed ones later); Odyle energy is one of them.
const COUNTERS_OPCODE: u32 = 0x0b61;
const COUNTERS_CHANGED_OPCODE: u32 = 0x0c61;
/// The counter that holds Odyle energy.
const ODYLE_KEY: i64 = 0xc987;
/// A removal reason of 0x4236: the entity died (1: loaded dead, 7: left the world).
const REMOVED_DIED: u8 = 3;
const REMOVED_GONE: u8 = 7;
const NOTICE_HOLD: Micros = 1_500_000;
const CALIBRATION_HITS: i32 = 300;
const MIN_SCALE: f64 = 1.0;
const MAX_SCALE: f64 = 40.0;
const DODGE_SKILL_ID: i32 = 11000100;
const MAX_PLAUSIBLE_AMOUNT: i64 = 100_000_000;
const RESTART_MIN_GAP: Micros = 90 * SECOND;
const RESTART_RADIUS: f64 = 2000.0;
const ZONE_KIND_MOVE: u32 = 2;
const OWNER_MARKER: [u8; 8] = [0xff; 8];

type NoticeKey = (i32, i32, i32);

pub struct FrameDecoder {
    protocol: Arc<Protocol>,
    pub entities: EntityDirectory,
    pub stats: DecoderStats,
    pub current_zone_id: i32,
    zone_changes: Vec<ZoneChange>,
    avoids: Vec<AvoidEvent>,
    entry_place: Option<(f32, f32)>,
    entry_at: Micros,
    spawned_at: HashMap<i32, Micros>,
    last_casts: HashMap<(i32, i32), (Micros, u64)>,
    cast_order: u64,
    pending_spawned: HashMap<i32, &'static str>,
    pending_leftovers: HashMap<i32, &'static str>,
    notices: HashMap<NoticeKey, VecDeque<Micros>>,
    notice_order: VecDeque<(Micros, NoticeKey, i64)>,
    estimated_targets: HashSet<i32>,
    exact_on_target: HashMap<i32, i64>,
    raw_estimate: HashMap<i32, (i64, i32)>,
    bundle_buffer: Vec<u8>,
    buffs: Vec<BuffEvent>,
    departures: Vec<Departure>,
    boss_reports: Vec<FieldBossReport>,
    /// Odyle energy of the own character: (basic, charged, when).
    pub odyle: Option<(u32, u32, Micros)>,
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<i64> {
    let mut value: i64 = 0;
    for i in 0..5 {
        let b = *data.get(*pos)?;
        *pos += 1;
        value |= ((b & 0x7f) as i64) << (7 * i);
        if b & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn u16_at(f: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([f[at], f[at + 1]])
}

fn u32_at(f: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]])
}

fn i64_at(f: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(f[at..at + 8].try_into().unwrap())
}

fn f32_at(f: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]])
}

/// A character name: 1-12 letters or digits, at least one letter (the tutorial's `$Kc03nyeQHr4` placeholders are not).
fn is_character_name(name: &str) -> bool {
    (1..=12).contains(&name.chars().count()) && name.chars().all(char::is_alphanumeric) && name.chars().any(char::is_alphabetic)
}

fn read_name(frame: &[u8], at: usize, min_length: usize) -> Option<String> {
    let length = *frame.get(at)? as usize;
    if length < min_length || length > 24 || at + 1 + length > frame.len() {
        return None;
    }
    let text = std::str::from_utf8(&frame[at + 1..at + 1 + length]).ok()?;
    text.chars().all(|c| (c as u32) <= 0xFFFF && (c.is_alphabetic() || c.to_digit(10).is_some())).then(|| text.to_string())
}

/// Movement frames of other entities: opcode (second byte 0x37) | entity (varint) | 1-2 header bytes | x, y, z (f32).
/// Returns where x starts after the entity id. In 0x1b, 0x1c, 0x1e, 0x28 and 0x29 an odd first header byte means a second
/// header byte (live EU capture 2026-10-15: of 9,000 such frames one reads plausibly at both offsets).
fn movement_offset(first: u8, header: u8) -> Option<usize> {
    match first {
        0x1a => Some(2),
        0x1f | 0x2f => Some(1),
        0x1b | 0x1c | 0x1e | 0x28 | 0x29 => Some(1 + (header & 1) as usize),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The hit kinds of a damage record. Layouts 6 and 7 carry them: uid | hit type | flags | restored HP (varint) | angle;
/// layouts 4 and 5 none. Flags: 0x01 block, 0x02 parry, 0x04 perfect, 0x08 double, 0x10 iron wall, 0x20 regeneration,
/// 0x40 perfect block (with block or parry; 0x80 mirrors a layout bit). Angle: 1 back, 2 front.
fn read_hit_kinds(frame: &[u8], p: usize, flags: u8) -> Option<HitKinds> {
    let kind = flags & 0xf;
    if kind == 4 || kind == 5 {
        return Some(HitKinds(0));
    }
    if !(6..=7).contains(&kind) || p + 5 > frame.len() {
        return None;
    }
    let bits = frame[p + 2];
    let mut at = p + 3;
    read_varint(frame, &mut at)?;
    let direction = *frame.get(at)?;
    let mut kinds = 0;
    for (bit, k) in [
        (0x01, HitKinds::BLOCK),
        (0x02, HitKinds::PARRY),
        (0x04, HitKinds::PERFECT),
        (0x08, HitKinds::DOUBLE),
        (0x10, HitKinds::IRON_WALL),
        (0x20, HitKinds::REGENERATION),
        (0x40, HitKinds::PERFECT_BLOCK),
    ] {
        if bits & bit != 0 {
            kinds |= k;
        }
    }
    if direction == 1 {
        kinds |= HitKinds::BACK;
    } else if direction == 2 {
        kinds |= HitKinds::FRONT;
    }
    Some(HitKinds(kinds))
}

impl FrameDecoder {
    pub fn new(protocol: Arc<Protocol>) -> Self {
        FrameDecoder {
            protocol,
            entities: EntityDirectory::new(),
            stats: DecoderStats::default(),
            current_zone_id: 0,
            zone_changes: Vec::new(),
            avoids: Vec::new(),
            entry_place: None,
            entry_at: 0,
            spawned_at: HashMap::new(),
            last_casts: HashMap::new(),
            cast_order: 0,
            pending_spawned: HashMap::new(),
            pending_leftovers: HashMap::new(),
            notices: HashMap::new(),
            notice_order: VecDeque::new(),
            estimated_targets: HashSet::new(),
            exact_on_target: HashMap::new(),
            raw_estimate: HashMap::new(),
            bundle_buffer: Vec::new(),
            buffs: Vec::new(),
            departures: Vec::new(),
            boss_reports: Vec::new(),
            odyle: None,
        }
    }

    pub fn protocol(&self) -> &Protocol {
        &self.protocol
    }

    pub fn drain_zone_changes(&mut self) -> Vec<ZoneChange> {
        std::mem::take(&mut self.zone_changes)
    }

    pub fn drain_avoids(&mut self) -> Vec<AvoidEvent> {
        std::mem::take(&mut self.avoids)
    }

    pub fn drain_buffs(&mut self) -> Vec<BuffEvent> {
        std::mem::take(&mut self.buffs)
    }

    pub fn drain_departures(&mut self) -> Vec<Departure> {
        std::mem::take(&mut self.departures)
    }

    pub fn drain_boss_reports(&mut self) -> Vec<FieldBossReport> {
        std::mem::take(&mut self.boss_reports)
    }

    pub fn decode(&mut self, frame: &[u8], timestamp: Micros, out: &mut Vec<DamageEvent>) {
        let first = out.len();
        self.decode_frame(frame, timestamp, false, out);
        if !self.estimated_targets.is_empty() {
            for e in &out[first..] {
                if !e.is_heal && self.estimated_targets.contains(&e.target) {
                    *self.exact_on_target.entry(e.target).or_default() += e.amount;
                }
            }
        }
        if self.notice_order.front().is_some_and(|n| n.0 <= timestamp - NOTICE_HOLD) {
            self.expire_notices(timestamp, out);
        }
    }

    fn expire_notices(&mut self, now: Micros, out: &mut Vec<DamageEvent>) {
        let cutoff = now - NOTICE_HOLD;
        while self.notice_order.front().is_some_and(|n| n.0 <= cutoff) {
            let (at, key, lost) = self.notice_order.pop_front().unwrap();
            let Some(queue) = self.notices.get_mut(&key) else { continue };
            if queue.pop_front().is_none() {
                continue;
            }
            if queue.is_empty() {
                self.notices.remove(&key);
            }
            if let Some(hit) = self.estimate_hit(at, key.0, key.1, key.2, lost) {
                out.push(hit);
            }
        }
    }

    fn estimate_hit(&mut self, at: Micros, actor: i32, target: i32, skill_id: i32, lost: i64) -> Option<DamageEvent> {
        let npc = self.entities.boss_npc_id_of(target)?;
        if !bosses::is_estimated_boss(npc)
            || self.entities.is_spawned(actor)
            || self.entities.is_known_player(target)
            || skills::is_non_damage_effect(skill_id)
            || skills::is_heal_family(skill_id)
        {
            return None;
        }
        let average = bosses::average_hit(skill_id)?;
        let source = self.entities.summon_owner_of(actor).unwrap_or(actor);
        if let Some(class) = skills::class_of(skill_id) {
            self.entities.note_class(source, class);
        }
        let entry = self.raw_estimate.entry(target).or_default();
        entry.0 += average;
        entry.1 += 1;
        let amount = ((average as f64 * self.estimate_scale(target, lost)).round_ties_even() as i64).max(1);
        let mut e = DamageEvent::new(at, source, target, amount, false, skills::name_of(skill_id));
        e.skill_id = skill_id;
        e.estimated = true;
        Some(e)
    }

    fn estimate_scale(&self, target: i32, lost: i64) -> f64 {
        let (raw, hits) = self.raw_estimate.get(&target).copied().unwrap_or_default();
        if hits < CALIBRATION_HITS || raw <= 0 {
            return 1.0;
        }
        let unexplained = lost - self.exact_on_target.get(&target).copied().unwrap_or(0);
        if unexplained <= 0 {
            1.0
        } else {
            (unexplained as f64 / raw as f64).clamp(MIN_SCALE, MAX_SCALE)
        }
    }

    fn decode_hit_notice(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(actor) = read_varint(frame, &mut p) else { return };
        if p >= frame.len() {
            return;
        }
        p += 1;
        let Some(target) = read_varint(frame, &mut p) else { return };
        if frame.len() < p + 4 {
            return;
        }
        let target = target as i32;
        match self.entities.boss_npc_id_of(target) {
            Some(npc) if bosses::is_estimated_boss(npc) => {}
            _ => return,
        }
        self.estimated_targets.insert(target);
        let key = (actor as i32, target, u32_at(frame, p) as i32);
        self.notices.entry(key).or_default().push_back(timestamp);
        let points = &self.entities.hit_points;
        let lost = match (points.highest_seen(target), points.latest(target)) {
            (Some(max), Some(latest)) if max > 0 => max - latest.hp,
            _ => 0,
        };
        self.notice_order.push_back((timestamp, key, lost));
    }

    fn take_notice(&mut self, key: NoticeKey) {
        if let Some(queue) = self.notices.get_mut(&key) {
            queue.pop_front();
            if queue.is_empty() {
                self.notices.remove(&key);
            }
        }
    }

    fn decode_frame(&mut self, frame: &[u8], timestamp: Micros, nested: bool, out: &mut Vec<DamageEvent>) {
        let protocol = self.protocol.clone();
        let layout = &protocol.frame;
        if frame.len() < layout.opcode_offset + layout.opcode_size {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let opcode = if layout.opcode_size == 2 {
            if layout.little_endian && !layout.opcode_big_endian {
                u16_at(frame, layout.opcode_offset) as u32
            } else {
                (frame[layout.opcode_offset] as u32) << 8 | frame[layout.opcode_offset + 1] as u32
            }
        } else {
            frame[layout.opcode_offset] as u32
        };
        self.stats.decoded_frames += 1;
        if self.stats.opcode_counts.len() < 4096 || self.stats.opcode_counts.contains_key(&opcode) {
            *self.stats.opcode_counts.entry(opcode).or_default() += 1;
        }

        if self.protocol.bundle_opcode == Some(opcode) && !nested {
            self.decode_bundle(frame, timestamp, out);
            return;
        }

        match opcode {
            ZONE_OPCODE => return self.decode_zone(frame, timestamp),
            HIT_NOTICE_OPCODE => return self.decode_hit_notice(frame, timestamp),
            BUFF_ON_OPCODE | BUFF_RENEWED_OPCODE => return self.decode_buff(frame, timestamp, opcode == BUFF_RENEWED_OPCODE),
            BUFF_OFF_OPCODE => return self.decode_buff_off(frame, timestamp),
            BOSS_MAP_OPCODE => return self.decode_boss_map(frame, timestamp),
            COUNTERS_OPCODE | COUNTERS_CHANGED_OPCODE => return self.decode_counters(frame, timestamp, opcode == COUNTERS_OPCODE),
            _ => {}
        }

        let family = self.protocol.family_of(opcode);
        match family {
            OpcodeFamily::Damage if self.protocol.damage_layout == "varint-v1" => self.decode_varint_damage(frame, timestamp, out),
            OpcodeFamily::HpUpdate if self.protocol.hp_layout == "varint-v1" => self.decode_hp(frame, timestamp),
            OpcodeFamily::Dot if self.protocol.dot_layout == "varint-v1" => self.decode_varint_dot(frame, timestamp, out),
            OpcodeFamily::Nickname if self.protocol.nickname_layout == "varint-v1" => self.decode_varint_nickname(frame),
            OpcodeFamily::Roster => self.decode_roster(frame, timestamp),
            OpcodeFamily::LocalPosition => self.decode_local_position(frame, timestamp),
            OpcodeFamily::Character => self.decode_character(frame, timestamp),
            OpcodeFamily::SelfStats => self.decode_self_stats(frame),
            OpcodeFamily::Appearance => self.decode_appearance(frame),
            OpcodeFamily::NpcSpawn => self.decode_npc_spawn(frame, timestamp),
            OpcodeFamily::NpcRemove => self.decode_npc_remove(frame, timestamp),
            OpcodeFamily::EnvSpawn => self.decode_env_spawn(frame, timestamp),
            OpcodeFamily::EnvRemove => self.decode_env_remove(frame),
            OpcodeFamily::Unknown if opcode & 0xff == 0x37 && movement_offset(frame[0], 0).is_some() => self.decode_movement(frame, timestamp),
            OpcodeFamily::Unknown => self.stats.unknown_opcodes += 1,
            _ => {}
        }
    }

    fn decode_bundle(&mut self, frame: &[u8], timestamp: Micros, out: &mut Vec<DamageEvent>) {
        let protocol = self.protocol.clone();
        let layout = &protocol.frame;
        let header_end = layout.opcode_offset + layout.opcode_size + 4;
        if !layout.is_varint() || frame.len() <= header_end {
            self.stats.bundle_failures += 1;
            return;
        }
        let size = u32_at(frame, layout.opcode_offset + layout.opcode_size) as usize;
        let mut raw = std::mem::take(&mut self.bundle_buffer);
        if size > lz4::MAX_OUTPUT || !lz4::decompress_into(&frame[header_end..], size, &mut raw) {
            self.stats.bundle_failures += 1;
            self.bundle_buffer = raw;
            return;
        }
        self.stats.bundles += 1;
        let mut p = 0;
        while p < raw.len() {
            let mut length: i64 = 0;
            let mut prefix = 0;
            let mut complete = false;
            while prefix < 5 && p + prefix < raw.len() {
                let b = raw[p + prefix];
                length |= ((b & 0x7f) as i64) << (7 * prefix);
                prefix += 1;
                if b & 0x80 == 0 {
                    complete = true;
                    break;
                }
            }
            let total = length + prefix as i64 + layout.length_bias;
            if !complete || total - (prefix as i64) < (layout.opcode_offset + layout.opcode_size) as i64 || p as i64 + total > raw.len() as i64 {
                self.stats.bundle_failures += 1;
                break;
            }
            let total = total as usize;
            self.decode_frame(&raw[p + prefix..p + total], timestamp, true, out);
            p += total;
        }
        self.bundle_buffer = raw;
    }

    fn decode_zone(&mut self, frame: &[u8], timestamp: Micros) {
        if frame.len() < 10 {
            return;
        }
        let zone_id = u32_at(frame, 6) as i32;
        if zone_id <= 0 {
            return;
        }
        let place = (frame.len() >= 26).then(|| (f32_at(frame, 18), f32_at(frame, 22)));
        let kind = if frame.len() >= 38 { u32_at(frame, 34) } else { 0 };
        if zone_id == self.current_zone_id {
            if let (ZONE_KIND_MOVE, Some(now), Some(entry)) = (kind, place, self.entry_place) {
                let distance = (((now.0 - entry.0) as f64).powi(2) + ((now.1 - entry.1) as f64).powi(2)).sqrt();
                if zones::is_instance(zone_id) && timestamp - self.entry_at >= RESTART_MIN_GAP && distance < RESTART_RADIUS {
                    self.entry_at = timestamp;
                    self.zone_changes.push(ZoneChange { timestamp, zone_id, restart: true });
                }
            }
            return;
        }
        self.current_zone_id = zone_id;
        self.entry_place = place;
        self.entry_at = timestamp;
        self.zone_changes.push(ZoneChange { timestamp, zone_id, restart: false });
    }

    /// Effect put on (0x2a38) or changed (0x2b38): target (varint) | [on: 01] | flags | instance (varint) | abnormal (u32) |
    /// length (i64, ms) | end (i64, unix ms) | caster (varint) | level (u8) | [flags & 2: skill (u32)] | 2 if re-applied |
    /// the caster's x, y, z (f32).
    fn decode_buff(&mut self, frame: &[u8], timestamp: Micros, renewed: bool) {
        let mut p = 2;
        let Some(target) = read_varint(frame, &mut p) else { return };
        if !(1..=10_000_000).contains(&target) || p + 2 > frame.len() {
            return;
        }
        if !renewed {
            if frame[p] != 1 {
                return;
            }
            p += 1;
        }
        let flags = frame[p];
        p += 1;
        let Some(instance) = read_varint(frame, &mut p) else { return };
        if p + 20 > frame.len() {
            return;
        }
        let effect = u32_at(frame, p);
        let (length_ms, end_ms) = (i64_at(frame, p + 4), i64_at(frame, p + 12));
        p += 20;
        let Some(caster) = read_varint(frame, &mut p) else { return };
        if !(1..=10_000_000).contains(&caster) || !(10..=2_999_999_999u32).contains(&effect) {
            return;
        }
        let caster = caster as i32;
        let mut q = p + 1;
        let skill = if flags & 2 != 0 && q + 4 <= frame.len() {
            q += 4;
            Some(u32_at(frame, q - 4))
        } else {
            None
        };
        if q + 13 == frame.len() {
            let change = if renewed { BuffChange::Changed { restarted: frame[q] == 2 } } else { BuffChange::On };
            self.buffs.push(BuffEvent { timestamp, target: target as i32, instance, abnormal: effect, caster, skill, length_ms, end_ms, change });
        }
        if frame.len() >= 12 && frame.len() - 12 > p && self.entities.is_local_player(caster) {
            let n = frame.len();
            let (x, y, z) = (f32_at(frame, n - 12), f32_at(frame, n - 8), f32_at(frame, n - 4));
            let plausible = |v: f32| v.is_finite() && v.abs() > 100.0 && v.abs() < 500_000.0;
            if plausible(x) && plausible(y) && z.is_finite() && z.abs() < 50_000.0 {
                self.entities.note_local_position(x, y, timestamp);
            }
        }
    }

    /// Effects taken off: target (varint) | count (u8) | count x (kind (u8) | instance (varint) | reason (u8) |
    /// [kind 7: by entity (varint) | skill (u32) | skill effect (u32)]).
    fn decode_buff_off(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(target) = read_varint(frame, &mut p) else { return };
        let Some(&count) = frame.get(p) else { return };
        p += 1;
        let mut removed = Vec::new();
        for _ in 0..count {
            let Some(&kind) = frame.get(p) else { return };
            p += 1;
            let Some(instance) = read_varint(frame, &mut p) else { return };
            let Some(&reason) = frame.get(p) else { return };
            p += 1;
            if kind == 7 {
                if read_varint(frame, &mut p).is_none() {
                    return;
                }
                p += 8;
            }
            removed.push((instance, reason));
        }
        if p != frame.len() {
            return;
        }
        for (instance, reason) in removed {
            let change = BuffChange::Off { reason };
            self.buffs.push(BuffEvent { timestamp, target: target as i32, instance, abnormal: 0, caster: 0, skill: None, length_ms: 0, end_ms: 0, change });
        }
    }

    /// The field bosses of a map, sent when the player opens it: 00 00 | map (u32) | count (varint) | count x (up (u8, 0 or 1) |
    /// boss (varint, map * 100 + n) | [up: x, y, z (f32)] | [a byte some bosses carry] | time (u64, unix ms)).
    fn decode_boss_map(&mut self, frame: &[u8], timestamp: Micros) {
        if frame.len() < 9 || frame[2] != 0 || frame[3] != 0 {
            return;
        }
        let map_id = u32_at(frame, 4);
        let mut p = 8;
        let Some(count) = read_varint(frame, &mut p).filter(|n| (1..=200).contains(n)) else { return };
        let now_ms = timestamp / 1000;
        let plausible = |t: i64| t == 0 || (now_ms - 14 * 86_400_000..=now_ms + 3 * 86_400_000).contains(&t);
        let entry_starts = |at: usize| {
            if at == frame.len() {
                return true;
            }
            let mut q = at + 1;
            frame.get(at).is_some_and(|&b| b <= 1) && read_varint(frame, &mut q).is_some_and(|id| (1000..=10_000_000).contains(&id))
        };
        let mut bosses = Vec::new();
        for i in 0..count {
            let Some(&up) = frame.get(p).filter(|&&b| b <= 1) else { return };
            p += 1;
            let Some(boss) = read_varint(frame, &mut p) else { return };
            let base = p + if up == 1 { 12 } else { 0 };
            let last = i + 1 == count;
            let found = [0usize, 1].into_iter().find_map(|extra| {
                let at = base + extra;
                let t = (at + 8 <= frame.len()).then(|| i64_at(frame, at))?;
                let end = at + 8;
                (plausible(t) && (last || entry_starts(end))).then_some((t, end))
            });
            let Some((time_ms, end)) = found else { return };
            bosses.push(FieldBossSighting { boss: boss as u32, up: up == 1, time_ms });
            p = end;
        }
        self.boss_reports.push(FieldBossReport { timestamp, map_id, bosses });
    }

    /// Character counters: entries kind (u8) | slot (u8) | key (varint) | values (varint), the kind telling which values are
    /// there (a zero is left out). Odyle energy is slot 1 of `ODYLE_KEY`: kind 0x0c basic and charged, 0x04 basic, 0x08 charged.
    /// All counters (0x0b61) have other fields first, so the entry is looked for, followed by slot 2 of the same key; changed
    /// ones (0x0c61) are a byte and the entries.
    fn decode_counters(&mut self, frame: &[u8], timestamp: Micros, all: bool) {
        // kind, slot, key, the energy if the kind is one of Odyle's, where the next entry starts
        let entry = |at: usize| -> Option<(u8, i64, (u32, u32), usize)> {
            let (kind, slot) = (*frame.get(at)?, *frame.get(at + 1)?);
            let mut p = at + 2;
            let key = read_varint(frame, &mut p)?;
            let n = match kind {
                0x0c => 2,
                0x04 | 0x08 => 1,
                _ => return None,
            };
            let mut values = [0i64; 2];
            for v in values.iter_mut().take(n) {
                *v = read_varint(frame, &mut p).filter(|v| (0..=100_000).contains(v))?;
            }
            let energy = match kind {
                0x0c => (values[0] as u32, values[1] as u32),
                0x04 => (values[0] as u32, 0),
                _ => (0, values[0] as u32),
            };
            Some((slot, key, energy, p))
        };
        let is_odyle = |slot: u8, key: i64| slot == 1 && key == ODYLE_KEY;
        let found = if all {
            (2..frame.len().saturating_sub(4)).find_map(|at| {
                let (_, _, energy, end) = entry(at).filter(|e| is_odyle(e.0, e.1))?;
                entry(end).is_some_and(|next| next.0 == 2 && next.1 == ODYLE_KEY).then_some(energy)
            })
        } else {
            let mut at = 3;
            let mut found = None;
            while let Some((slot, key, energy, end)) = entry(at) {
                if is_odyle(slot, key) {
                    found = Some(energy);
                    break;
                }
                at = end;
            }
            found
        };
        if let Some((basic, charged)) = found {
            self.odyle = Some((basic, charged, timestamp));
        }
    }

    fn decode_env_spawn(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(entity) = read_varint(frame, &mut p) else { return };
        if frame.len() < p + 18 {
            return;
        }
        let env_id = u32_at(frame, p + 2) as i64;
        let what = if gather::is_hidden_cube(env_id) { Some(("Cube", "Hidden Cube")) } else { gather::lookup(env_id) };
        let Some((kind, key)) = what else { return };
        let (x, y, z) = (f32_at(frame, p + 6), f32_at(frame, p + 10), f32_at(frame, p + 14));
        let z = if z.is_finite() && z.abs() < 1_000_000.0 { z } else { f32::NAN };
        if x.is_finite() && y.is_finite() && x.abs() < 1_000_000.0 && y.abs() < 1_000_000.0 {
            self.entities.note_env(entity as i32, kind, key, x, y, z, timestamp);
        }
    }

    fn decode_env_remove(&mut self, frame: &[u8]) {
        let mut p = 2;
        if let Some(entity) = read_varint(frame, &mut p) {
            self.entities.forget_env(entity as i32);
        }
    }

    fn decode_npc_remove(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(entity) = read_varint(frame, &mut p) else { return };
        if frame.len() != p + 2 {
            return;
        }
        let reason = frame[frame.len() - 1];
        if reason == REMOVED_DIED || reason == REMOVED_GONE {
            self.departures.push(Departure { timestamp, entity: entity as i32, died: reason == REMOVED_DIED });
        }
        self.entities.note_removed(entity as i32, reason, timestamp);
    }

    fn decode_npc_spawn(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(entity) = read_varint(frame, &mut p) else { return };
        if frame.len() < p + 7 {
            return;
        }
        let entity = entity as i32;
        self.spawned_at.insert(entity, timestamp);

        let mut owner_name = None;
        if frame[p + 2] == 1 {
            if let Some(named) = read_name(frame, p + 3, 2) {
                owner_name = Some(named);
                p += 1 + frame[p + 3] as usize;
                if frame.len() < p + 7 {
                    return;
                }
            }
        }

        p += 3;
        let npc = u32_at(frame, p) as i32;
        self.entities.note_spawned(entity);
        self.entities.register_npc(entity, npc);
        self.entities.set_summon_owner_name(entity, owner_name);
        if frame.len() >= p + 14 {
            let (mx, my) = (f32_at(frame, p + 6), f32_at(frame, p + 10));
            let mz = if frame.len() >= p + 18 { f32_at(frame, p + 14) } else { f32::NAN };
            let mz = if mz.is_finite() && mz.abs() < 1_000_000.0 { mz } else { f32::NAN };
            if mx.is_finite() && my.is_finite() && mx.abs() < 1_000_000.0 && my.abs() < 1_000_000.0 {
                self.entities.note_mob_position(entity, npc, mx, my, mz, timestamp);
            }
        }

        if let Some(marker) = find(&frame[p + 4..], &OWNER_MARKER) {
            let mut q = p + 4 + marker + OWNER_MARKER.len() + 8;
            if q < frame.len() {
                if let Some(owner) = read_varint(frame, &mut q) {
                    if owner > 0 {
                        let owner = owner as i32;
                        self.entities.set_summon_owner(entity, (owner != entity).then_some(owner));
                    }
                }
            }
        }
    }

    fn guess_summon_owner(&mut self, actor: i32, skill_id: i32, target: i32) -> Option<i32> {
        if !self.entities.is_spawned(actor) || self.entities.is_known_player(target) || self.entities.summon_owner_of(target).is_some() {
            return None;
        }
        let class = skills::class_of(skill_id)?;
        let variant = skill_id / 10;
        let party: Vec<i32> = self.entities.party_member_ids_of_class(class).into_iter().filter(|&id| id != actor).collect();
        let mut owner = self.owner_by_cast(actor, variant, 5 * SECOND, |_, id| party.contains(&id));
        if owner.is_none() {
            owner = self.owner_by_cast(actor, variant, 2 * SECOND, |e, id| !e.is_named_outside_party(id));
        }
        if owner.is_none() && party.len() == 1 {
            owner = Some(party[0]);
        }
        match owner {
            Some(found) => {
                self.pending_spawned.remove(&actor);
                self.entities.set_summon_owner(actor, Some(found));
            }
            None if party.is_empty() && self.pending_spawned.len() < 64 => {
                self.pending_spawned.insert(actor, class);
            }
            None => {}
        }
        owner
    }

    fn owner_by_cast(&self, summon: i32, variant: i32, window: Micros, eligible: impl Fn(&EntityDirectory, i32) -> bool) -> Option<i32> {
        let spawned = *self.spawned_at.get(&summon)?;
        self.last_casts
            .iter()
            .filter(|((caster, v), _)| *v == variant && eligible(&self.entities, *caster))
            .map(|((caster, _), (at, order))| (*caster, spawned - at, *order))
            .filter(|(_, gap, _)| *gap >= 0 && *gap <= window)
            .min_by_key(|(_, gap, order)| (*gap, *order))
            .map(|(id, _, _)| id)
    }

    fn note_cast(&mut self, actor: i32, skill_id: i32, timestamp: Micros) {
        if !self.entities.is_spawned(actor) && skills::class_of(skill_id).is_some() {
            let order = self.cast_order;
            let entry = self.last_casts.entry((actor, skill_id / 10)).or_insert((timestamp, order));
            if entry.1 == order {
                self.cast_order += 1;
            }
            entry.0 = timestamp;
        }
    }

    fn leftover_summon_owner(&mut self, actor: i32, skill_id: i32) -> Option<i32> {
        if self.entities.is_spawned(actor) || self.entities.has_name(actor) {
            return None;
        }
        let class = skills::summon_attack_class(skill_id)?;
        let owners: Vec<i32> = self.entities.party_player_ids_of_class(class).into_iter().filter(|&id| id != actor).collect();
        if owners.len() != 1 {
            if owners.is_empty() && self.pending_leftovers.len() < 64 {
                self.pending_leftovers.insert(actor, class);
            }
            return None;
        }
        self.pending_leftovers.remove(&actor);
        self.entities.set_summon_owner(actor, Some(owners[0]));
        Some(owners[0])
    }

    fn resolve_pending_leftovers(&mut self) {
        let spawned: Vec<(i32, &'static str)> = self.pending_spawned.iter().map(|(a, c)| (*a, *c)).collect();
        for (actor, class) in spawned {
            if self.entities.summon_owner_of(actor).is_some() || self.entities.has_name(actor) {
                self.pending_spawned.remove(&actor);
                continue;
            }
            let owners: Vec<i32> = self.entities.party_player_ids_of_class(class).into_iter().filter(|&id| id != actor).collect();
            if owners.len() == 1 {
                self.pending_spawned.remove(&actor);
                self.entities.set_summon_owner(actor, Some(owners[0]));
            }
        }

        let leftovers: Vec<(i32, &'static str)> = self.pending_leftovers.iter().map(|(a, c)| (*a, *c)).collect();
        for (actor, class) in leftovers {
            if self.entities.is_spawned(actor) || self.entities.has_name(actor) || self.entities.summon_owner_of(actor).is_some() {
                self.pending_leftovers.remove(&actor);
                continue;
            }
            let owners: Vec<i32> = self.entities.party_player_ids_of_class(class).into_iter().filter(|&id| id != actor).collect();
            if owners.len() == 1 {
                self.pending_leftovers.remove(&actor);
                self.entities.set_summon_owner(actor, Some(owners[0]));
            }
        }
    }

    fn decode_varint_damage(&mut self, frame: &[u8], timestamp: Micros, out: &mut Vec<DamageEvent>) {
        if !self.pending_leftovers.is_empty() || !self.pending_spawned.is_empty() {
            self.resolve_pending_leftovers();
        }

        let mut p = 2;
        let Some(target) = read_varint(frame, &mut p) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        if frame.len() < p + 2 {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let target = target as i32;
        let flags = frame[p];
        p += 2;
        if flags & 0x04 == 0 {
            self.stats.no_damage_frames += 1;
            if let Some(caster) = read_varint(frame, &mut p) {
                if frame.len() >= p + 4 {
                    self.note_cast(caster as i32, u32_at(frame, p) as i32, timestamp);
                }
            }
            return;
        }

        let Some(actor) = read_varint(frame, &mut p) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        if frame.len() < p + 6 {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let actor = actor as i32;
        let skill_id = u32_at(frame, p) as i32;
        p += 4;
        self.take_notice((actor, target, skill_id));
        self.note_cast(actor, skill_id, timestamp);
        if skills::is_non_damage_effect(skill_id) {
            return;
        }

        let critical = frame[p + 1] == 3;
        let kinds = read_hit_kinds(frame, p, flags);

        let mut marker = None;
        let mut i = p + 2;
        while i + 4 <= frame.len() {
            if (1..=9).contains(&frame[i]) && frame[i + 1] == 0 && frame[i + 2] == 0 && frame[i + 3] == 0 {
                marker = Some(i);
                break;
            }
            i += 1;
        }

        let amount = marker.and_then(|m| {
            let mut q = m + 4;
            read_varint(frame, &mut q)?;
            if q >= frame.len() {
                return None;
            }
            read_varint(frame, &mut q)
        });
        let Some(amount) = amount.filter(|a| *a > 0 && *a <= MAX_PLAUSIBLE_AMOUNT) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        let hit_count = marker.map_or(1, |m| (frame[m] as i32).clamp(1, 9));

        if skill_id == DODGE_SKILL_ID {
            self.avoids.push(AvoidEvent { timestamp, source: actor, target });
            return;
        }

        let source = match self.entities.summon_owner_of(actor) {
            Some(owner) => owner,
            None => self.guess_summon_owner(actor, skill_id, target).or_else(|| self.leftover_summon_owner(actor, skill_id)).unwrap_or(actor),
        };
        if let Some(class) = skills::class_of(skill_id) {
            self.entities.note_class(source, class);
        }

        let skill = skills::name_of(skill_id);
        let is_heal = skills::is_heal_family(skill_id) && !self.entities.is_known_monster(target);
        if !is_heal && self.entities.is_known_player(source) && !self.entities.is_known_player(target) && self.entities.summon_owner_of(target).is_none() {
            self.entities.note_monster_hit(source, target, skill_id);
            if self.entities.is_local_player(source) {
                self.entities.note_local_hit(target, timestamp);
            }
        }
        if !is_heal && self.entities.is_local_player(target) && !self.entities.is_known_player(source) && self.entities.summon_owner_of(source).is_none() {
            self.entities.note_attacked_local(source, timestamp);
        }

        if is_heal && self.entities.summon_owner_of(target).is_some() {
            return;
        }

        let mut e = DamageEvent::new(timestamp, source, target, amount, is_heal, skill);
        e.is_critical = critical && !is_heal;
        e.skill_id = skill_id;
        e.kinds = if is_heal { None } else { kinds };
        e.hit_count = hit_count;
        out.push(e);
    }

    fn decode_varint_dot(&mut self, frame: &[u8], timestamp: Micros, out: &mut Vec<DamageEvent>) {
        let mut p = 2;
        let Some(target) = read_varint(frame, &mut p) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        if p >= frame.len() {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let target = target as i32;
        let flags = frame[p];
        p += 1;
        if flags & 0x02 == 0 || flags & 0x08 == 0 {
            return;
        }
        let (Some(actor), Some(_)) = (read_varint(frame, &mut p), read_varint(frame, &mut p)) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        if frame.len() < p + 4 {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let actor = actor as i32;
        let effect_id = u32_at(frame, p);
        p += 4;
        let Some(amount) = read_varint(frame, &mut p) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        let mut healed = 0;
        if flags & 0x01 != 0 {
            match read_varint(frame, &mut p) {
                Some(h) => healed = h,
                None => {
                    self.stats.skipped_short_frames += 1;
                    return;
                }
            }
        }
        if frame.len() != p + 4 {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let skill_id = u32_at(frame, p) as i32;

        if effect_id < 1_000_000_000 && skills::class_of(skill_id).is_some() {
            let effect_skill = (effect_id / 100) as i32;
            if amount > 0 && amount <= MAX_PLAUSIBLE_AMOUNT {
                let mut e = DamageEvent::new(timestamp, target, target, amount, true, skills::name_of(effect_skill));
                e.is_tick = true;
                e.skill_id = effect_skill;
                out.push(e);
            }
            return;
        }

        let source = self.entities.summon_owner_of(actor).unwrap_or(actor);
        if skills::is_heal_family(skill_id) || (self.entities.is_known_player(source) && self.entities.is_known_player(target)) {
            let counted = flags & 0x01 != 0
                && healed > 0
                && healed <= MAX_PLAUSIBLE_AMOUNT
                && skills::class_of(skill_id).is_some()
                && self.entities.summon_owner_of(target).is_none();
            if counted {
                let mut e = DamageEvent::new(timestamp, source, target, healed, true, skills::name_of(skill_id));
                e.is_tick = true;
                e.skill_id = skill_id;
                out.push(e);
            }
            return;
        }

        if amount <= 0 || amount > MAX_PLAUSIBLE_AMOUNT || target == actor {
            return;
        }
        let mut e = DamageEvent::new(timestamp, source, target, amount, false, skills::name_of(skill_id));
        e.is_tick = true;
        e.skill_id = skill_id;
        out.push(e);
    }

    fn decode_hp(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(entity) = read_varint(frame, &mut p) else {
            self.stats.skipped_short_frames += 1;
            return;
        };
        if p >= frame.len() {
            self.stats.skipped_short_frames += 1;
            return;
        }
        let entity = entity as i32;
        let format = frame[p];
        p += 1;
        if format & 1 != 0 {
            self.entities.note_detailed_stats(entity);
            if p >= frame.len() {
                self.stats.skipped_short_frames += 1;
                return;
            }
            p += 1 + frame[p] as usize * 5;
        }

        let mut current = None;
        let mut reported_max = None;
        if format & 2 != 0 {
            if p >= frame.len() {
                self.stats.skipped_short_frames += 1;
                return;
            }
            let count = frame[p] as usize;
            p += 1;
            let mut i = 0;
            while i < count && p + 9 <= frame.len() {
                match frame[p] {
                    0 => current = Some(i64_at(frame, p + 1)),
                    7 => reported_max = Some(i64_at(frame, p + 1)),
                    _ => {}
                }
                i += 1;
                p += 9;
            }
        }

        if p != frame.len() || format & !3 != 0 {
            self.stats.skipped_short_frames += 1;
            return;
        }

        if let Some(max) = reported_max.filter(|m| *m > 0) {
            self.entities.hit_points.note_max(entity, max);
        }
        if let Some(hp) = current.filter(|h| *h >= 0) {
            self.entities.hit_points.note(entity, timestamp, hp);
            if hp == 0 {
                self.entities.clear_local_target(entity);
                self.entities.forget_live_mob(entity);
            }
        }
    }

    fn decode_movement(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(entity) = read_varint(frame, &mut p) else { return };
        if !(1..=10_000_000).contains(&entity) || frame.len() <= p {
            return;
        }
        let Some(offset) = movement_offset(frame[0], frame[p]) else { return };
        if frame.len() < p + offset + 12 {
            return;
        }
        let (x, y, z) = (f32_at(frame, p + offset), f32_at(frame, p + offset + 4), f32_at(frame, p + offset + 8));
        let plausible = |v: f32| v.is_finite() && v.abs() > 1.0 && v.abs() < 500_000.0;
        if plausible(x) && plausible(y) && z.is_finite() && z.abs() < 100_000.0 {
            self.entities.note_movement(entity as i32, x, y, z, timestamp);
        }
    }

    fn decode_local_position(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(entity) = read_varint(frame, &mut p) else { return };
        if frame.len() < p + 9 || frame[p] != 3 {
            return;
        }
        if !self.entities.is_local_player(entity as i32) {
            return;
        }
        let (x, y) = (f32_at(frame, p + 1), f32_at(frame, p + 5));
        if x.is_finite() && y.is_finite() && x.abs() < 1_000_000.0 && y.abs() < 1_000_000.0 {
            self.entities.note_local_position(x, y, timestamp);
        }
    }

    fn decode_varint_nickname(&mut self, frame: &[u8]) {
        let mut p = 2;
        let Some(id) = read_varint(frame, &mut p) else { return };
        let id = id as i32;
        let end = (frame.len() as i64 - 4).min(p as i64 + 24);
        let mut k = p;
        while (k as i64) < end {
            if let Some(name) = read_name(frame, k, 3) {
                self.entities.register(id, &name);
                let after = k + 1 + frame[k] as usize;
                let faction_at = after + 4;
                if faction_at < frame.len() && matches!(frame[faction_at], 1 | 2) {
                    self.entities.note_faction(id, frame[faction_at]);
                }
                let mut i = after;
                while i + 11 < frame.len() {
                    let server = u16_at(frame, i);
                    if (1000..=9999).contains(&server)
                        && frame[i + 8] == frame[i]
                        && frame[i + 9] == frame[i + 1]
                        && (frame[i + 2] | frame[i + 3] | frame[i + 4] | frame[i + 5]) != 0
                        && frame[i + 6] == 0
                        && frame[i + 7] == 0
                    {
                        if let Some(other) = read_name(frame, i + 10, 2).filter(|o| *o != name) {
                            self.entities.note_non_player_name(&other);
                            self.entities.set_guild(id, other);
                            break;
                        }
                    }
                    i += 1;
                }
                return;
            }
            k += 1;
        }
    }

    fn decode_appearance(&mut self, frame: &[u8]) {
        let mut p = 2;
        if read_varint(frame, &mut p).is_none() || frame.len() < p + 4 {
            return;
        }
        p += 4;
        let Some(id) = read_varint(frame, &mut p) else { return };
        if id <= 0 || frame.len() < p + 3 {
            return;
        }
        let Some(name) = read_name(frame, p + 2, 2) else { return };
        let id = id as i32;
        self.entities.register(id, &name);
        if let Some(guild) = read_name(frame, p + 3 + frame[p + 2] as usize, 2).filter(|g| *g != name) {
            self.entities.note_non_player_name(&guild);
            self.entities.set_guild(id, guild);
        }
    }

    /// The own character record (zone load and every few minutes): opcode | combat id (varint) | mask (u32) | flags (u8, bit 0:
    /// a name follows) | name (u8 length + UTF-8) | server (u16) | class (u32, roster code) | u8 | level (u32).
    /// Live EU capture 2026-10-17: `33 36 db 43 5f a1 c1 28 37 03 "Kat" 63 09 1c 00 00 00 02 2d 00 00 00`.
    fn decode_character(&mut self, frame: &[u8], timestamp: Micros) {
        let mut p = 2;
        let Some(id) = read_varint(frame, &mut p) else { return };
        if !(1..=9_999_999).contains(&id) || frame.len() < p + 6 || frame[p + 4] & 1 == 0 {
            return;
        }
        let at = p + 5;
        let length = frame[at] as usize;
        if !(1..=48).contains(&length) || at + 1 + length + 2 > frame.len() {
            return;
        }
        let Some(name) = std::str::from_utf8(&frame[at + 1..at + 1 + length]).ok().filter(|n| is_character_name(n)) else { return };
        let after = at + 1 + length;
        let server_id = u16_at(frame, after);
        if !(1000..3000).contains(&server_id) {
            return;
        }
        let class_code = if after + 6 <= frame.len() { u32_at(frame, after + 2) as i32 } else { 0 };
        let level = if after + 11 <= frame.len() { u32_at(frame, after + 7) as i32 } else { 0 };
        self.entities.set_local_character(CharacterInfo {
            combat_id: id as i32,
            name: name.to_string(),
            class_code,
            level: if (1..=200).contains(&level) { level } else { 0 },
            server_id,
            received_at: timestamp,
        });
    }

    /// Own stats: opcode | combat id (varint) | ...; sent only about the local player, from the moment a zone loads.
    fn decode_self_stats(&mut self, frame: &[u8]) {
        let mut p = 2;
        if let Some(id) = read_varint(frame, &mut p).filter(|id| (100..=9_999_999).contains(id)) {
            self.entities.note_self_stats(id as i32);
        }
    }

    fn decode_roster(&mut self, frame: &[u8], timestamp: Micros) {
        let party_frame = frame.len() >= 2 && frame[0] == 0x02 && frame[1] == 0x97;
        let mut members = Vec::new();
        let mut i = 2;
        while i + 1 < frame.len() {
            let Some(name) = read_name(frame, i, 2) else {
                i += 1;
                continue;
            };
            let after = i + 1 + frame[i] as usize;
            if after + 8 > frame.len()
                || frame[after + 1] != 0
                || frame[after + 2] != 0
                || frame[after + 3] != 0
                || frame[after + 5] != 0
                || frame[after + 6] != 0
                || frame[after + 7] != 0
            {
                i += 1;
                continue;
            }
            let level = frame[after + 4];
            if frame[after] == 0 || !(1..=60).contains(&level) {
                i += 1;
                continue;
            }
            self.entities.note_roster_name(&name);
            if party_frame {
                if let Some(class) = skills::class_from_roster_code(frame[after] as i32) {
                    self.entities.note_party_class(&name, class);
                }
                if let Some(stats) = roster_stats(frame, i, after) {
                    self.entities.note_party_stats(&name, stats);
                }
            }
            members.push(name);
            i = after + 8;
        }
        if party_frame && !members.is_empty() {
            self.entities.note_party(&members, timestamp);
        }
    }
}

/// Gear score and combat power of the party roster member whose name length byte is at `name_at` and whose
/// class field starts at `after`. Layout (as A2Tools-DPS-Meter reads it): `dbid u64 | name | class u32 | level u32 |
/// gear score u32 | world id u16, u16 | tag u8 | combat power u64`. The bytes between the gear score and the world id
/// vary in width, so the world id (the top u16 of `dbid`, right before the name) is searched for as the anchor.
fn roster_stats(frame: &[u8], name_at: usize, after: usize) -> Option<PartyStats> {
    let gear_at = after + 8;
    if name_at < 8 || gear_at + 4 > frame.len() {
        return None;
    }
    let world = u16_at(frame, name_at - 2);
    let gear_score = u32_at(frame, gear_at);
    if world == 0 || gear_score > 1_000_000 {
        return None;
    }
    let start = gear_at + 4;
    let anchor = (start..start + 10).find(|&k| k + 2 <= frame.len() && u16_at(frame, k) == world)?;
    let power_at = anchor + 5;
    if power_at + 8 > frame.len() {
        return Some(PartyStats { gear_score, combat_power: None });
    }
    let power = i64_at(frame, power_at);
    let combat_power = (0..=100_000_000).contains(&power).then_some(power as u64);
    Some(PartyStats { gear_score, combat_power })
}

#[cfg(test)]
mod roster_tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    /// Slots 1 and 5 filled, 2-4 vacant (A2Tools-DPS-Meter test data, from a 2026-08-15 capture).
    #[test]
    fn roster_reads_gear_score_and_combat_power() {
        let frame = hex(&[
            "02 97 a5 f9 08 00 0b 38 35 30 4b 20 e8 bf 9e e5 88 b7 05 63 28 09 00 00 03 85 4b 01 00 00 00 f6 03 1f 02 00 05",
            "0c 01 85 4b 01 00 00 00 f6 03 0f e4 b9 9d e5 b7 9e e4 be 9d e7 84 b6 e5 9c a8 06 00 00 00 32 00 00 00 07 17 00 00 f6 03 f6 03 04 38 39 0d 00 00 00 00 00 00 01 01",
            "00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00",
            "00 03 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00",
            "00 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00",
            "0e 05 62 64 01 00 00 00 f6 03 02 4d 37 0e 00 00 00 32 00 00 00 5e 16 00 00 f6 03 f6 03 04 6a 03 0d 00 00 00 00 00 00 01 01",
            "02 0e 00 36",
        ]
        .join(" "));
        let mut decoder = FrameDecoder::new(Arc::new(Protocol::load()));
        decoder.decode_roster(&frame, 0);
        assert_eq!(decoder.entities.party_stats("九州依然在"), Some(PartyStats { gear_score: 5895, combat_power: Some(866_616) }));
        assert_eq!(decoder.entities.party_stats("M7"), Some(PartyStats { gear_score: 5726, combat_power: Some(852_842) }));
    }

    #[test]
    fn character_record_names_the_local_player() {
        let frame = hex("33 36 db 43 5f a1 c1 28 37 03 4b 61 74 63 09 1c 00 00 00 02 2d 00 00 00 bf 07 00 00 bf 07 00 00");
        let mut decoder = FrameDecoder::new(Arc::new(Protocol::load()));
        decoder.decode_character(&frame, 0);
        let c = decoder.entities.local_character().expect("character");
        assert_eq!((c.combat_id, c.name.as_str(), c.server_id, c.class_code, c.level), (8667, "Kat", 2403, 28, 45));
        assert_eq!(decoder.entities.name_for(8667).as_deref(), Some("Kat"));

        // a zone change: the own-stats records move the character to its new combat id
        let stats = hex("4a 36 a2 1f 00 00 00 00 00 00 00 00 00");
        for _ in 0..3 {
            decoder.decode_self_stats(&stats);
        }
        assert_eq!(decoder.entities.local_player_id(), Some(4002));
        assert_eq!(decoder.entities.name_for(4002).as_deref(), Some("Kat"));
    }

    #[test]
    fn boss_map_gives_each_boss_its_time() {
        // the first two of Altgard's 24 bosses, live EU capture 2026-10-17; the first carries the extra byte 80
        let frame = hex("01 91 00 00 56 04 00 00 02 00 a9 e3 06 80 ce 12 b1 48 a1 01 00 00 00 9e e3 06 7f f9 38 48 a1 01 00 00");
        let mut decoder = FrameDecoder::new(Arc::new(Protocol::load()));
        decoder.decode_boss_map(&frame, 1_792_211_419_284_629);
        let reports = decoder.drain_boss_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].map_id, 1110);
        assert_eq!(reports[0].bosses[0], FieldBossSighting { boss: 111017, up: false, time_ms: 1_792_220_926_670 });
        assert_eq!(reports[0].bosses[1], FieldBossSighting { boss: 111006, up: false, time_ms: 1_792_213_055_871 });
    }

    #[test]
    fn odyle_energy_from_the_counters() {
        let mut decoder = FrameDecoder::new(Arc::new(Protocol::load()));
        // all counters at login (live EU capture 2026-10-17, cut): basic 15, charged 340
        decoder.decode_counters(&hex("0b 61 49 04 01 00 00 00 0c 01 87 93 03 0f d4 02 04 02 87 93 03 0e 04 03 87 93 03 0e"), 5, true);
        assert_eq!(decoder.odyle, Some((15, 340, 5)));
        // a change: basic 30, charged 880
        decoder.decode_counters(&hex("0c 61 01 0c 01 87 93 03 1e f0 06 01 28 00 00 00"), 6, false);
        assert_eq!(decoder.odyle, Some((30, 880, 6)));
        // another counter changed: the energy stays
        decoder.decode_counters(&hex("0c 61 00 04 0c 00 00 00 0e 02"), 7, false);
        assert_eq!(decoder.odyle, Some((30, 880, 6)));
    }

    #[test]
    fn buff_records_open_and_close_effects() {
        let mut decoder = FrameDecoder::new(Arc::new(Protocol::load()));
        // Benediction on 6759 by itself, then taken off when it ran out
        decoder.decode_buff(
            &hex("2a 38 e7 34 01 13 82 02 e1 65 a6 09 10 27 00 00 00 00 00 00 bc 5c ba 14 a1 01 00 00 e7 34 03 30 0a f7 00 00 00 00 00 00 00 00 00 00 00 00 00 00"),
            1,
            false,
        );
        decoder.decode_buff_off(&hex("2c 38 e7 34 01 00 82 02 01"), 2);
        let buffs = decoder.drain_buffs();
        assert_eq!(buffs.len(), 2);
        assert_eq!((buffs[0].target, buffs[0].instance, buffs[0].abnormal, buffs[0].caster), (6759, 258, 161_900_001, 6759));
        assert_eq!((buffs[0].length_ms, buffs[0].skill, buffs[0].change), (10_000, Some(16_190_000), BuffChange::On));
        assert_eq!((buffs[1].instance, buffs[1].change), (258, BuffChange::Off { reason: 1 }));
    }

    #[test]
    fn placeholder_names_are_not_characters() {
        assert!(is_character_name("Kat") && is_character_name("九州依然在"));
        assert!(!is_character_name("$Kc03nyeQHr4") && !is_character_name("12345") && !is_character_name(""));
    }
}

#[cfg(test)]
mod movement_tests {
    use super::movement_offset;

    #[test]
    fn header_bit_selects_offset() {
        // 1c37 a39103 | 05 03 | x ... and 1c37 89c402 | 04 | x ... from a live capture
        assert_eq!(movement_offset(0x1c, 0x05), Some(2));
        assert_eq!(movement_offset(0x1c, 0x04), Some(1));
        assert_eq!(movement_offset(0x29, 0x02), Some(1));
        assert_eq!(movement_offset(0x29, 0x03), Some(2));
        assert_eq!(movement_offset(0x1a, 0x00), Some(2));
        assert_eq!(movement_offset(0x2f, 0x02), Some(1));
        assert_eq!(movement_offset(0x2a, 0x16), None);
        assert_eq!(movement_offset(0x3c, 0x03), None);
    }
}
