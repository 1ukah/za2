use std::collections::HashMap;
use std::sync::Arc;

use crate::buffs::BuffTimeline;
use crate::catalog::bosses;
use crate::field_bosses::FieldBossBoard;
use crate::combat::CombatLog;
use crate::segment::SECOND;

/// A pause in combat this long ends a fight; the next hit starts a new one.
pub const FIGHT_GAP: crate::segment::Micros = 10 * SECOND;
/// A boss fight this short (a stray hit) is not saved.
const MIN_BOSS_FIGHT: crate::segment::Micros = 10 * SECOND;
use crate::decoder::{DamageEvent, FrameDecoder, ZoneChange};
use crate::ping::PingTracker;
use crate::protocol::Protocol;
use crate::reassembler::TcpReassembler;
use crate::segment::{Micros, TcpSegment};
use crate::sessions::{self, SessionSummary};

pub struct Engine {
    protocol: Arc<Protocol>,
    pub reassembler: TcpReassembler,
    pub decoder: FrameDecoder,
    pub ping: PingTracker,
    /// Everything since the zone was entered or the meter reset (a whole dungeon run).
    pub combat: CombatLog,
    /// The current fight: starts over when combat resumes after a pause of more than `FIGHT_GAP`.
    pub fight: CombatLog,
    pub buffs: BuffTimeline,
    pub field_bosses: FieldBossBoard,
    /// Set when a map report or a field boss kill changed `field_bosses`.
    pub field_bosses_changed: bool,
    pub segments: u64,
    pub events: u64,
    pub last_segment_at: Option<Micros>,
    pub reset_on_zone_change: bool,
    zone_changes: Vec<ZoneChange>,
    session_start: Option<Micros>,
    zone_of_session: i32,
    finished: Vec<SessionSummary>,
    scratch: Vec<DamageEvent>,
}

impl Engine {
    pub fn new(protocol: Protocol) -> Engine {
        let protocol = Arc::new(protocol);
        Engine {
            protocol: protocol.clone(),
            reassembler: TcpReassembler::new(),
            decoder: FrameDecoder::new(protocol),
            ping: PingTracker::new(),
            combat: CombatLog::new(),
            fight: CombatLog::new(),
            buffs: BuffTimeline::new(),
            field_bosses: FieldBossBoard::default(),
            field_bosses_changed: false,
            segments: 0,
            events: 0,
            last_segment_at: None,
            reset_on_zone_change: true,
            zone_changes: Vec::new(),
            session_start: None,
            zone_of_session: 0,
            finished: Vec::new(),
            scratch: Vec::new(),
        }
    }

    pub fn ingest(&mut self, segment: &TcpSegment) {
        self.scratch.clear();
        if !segment.from_server {
            self.ping.on_client_payload(&segment.payload, segment.timestamp);
            return;
        }
        self.segments += 1;
        self.last_segment_at = Some(segment.timestamp);
        self.session_start.get_or_insert(segment.timestamp);
        let decoder = &mut self.decoder;
        let ping = &mut self.ping;
        let scratch = &mut self.scratch;
        let protocol = &self.protocol;
        self.reassembler.push(segment, &protocol.frame, &protocol.sync_opcodes, &mut |frame| {
            ping.on_server_frame(frame, segment.timestamp);
            decoder.decode(frame, segment.timestamp, scratch)
        });
        self.events += self.scratch.len() as u64;

        // Before a zone change saves the session, so the session's events sit on the local player's current id.
        self.apply_local_id_changes();
        for change in self.decoder.drain_zone_changes() {
            if self.reset_on_zone_change {
                self.finish_session(change.timestamp);
            }
            self.buffs.zone_change(self.decoder.entities.local_player_id(), change.timestamp);
            self.zone_changes.push(change);
        }
        for e in self.decoder.drain_buffs() {
            self.buffs.note(&e);
        }
        self.buffs.sweep(segment.timestamp);
        for d in self.decoder.drain_departures() {
            self.buffs.depart(d.entity, d.timestamp);
            let entities = &self.decoder.entities;
            if d.died && entities.is_known_player(d.entity) {
                self.combat.note_death(d.entity, d.timestamp);
                self.fight.note_death(d.entity, d.timestamp);
            }
            if d.died && entities.npc_id_of(d.entity).is_some_and(|npc| self.field_bosses.note_kill(npc, d.timestamp / 1000)) {
                self.field_bosses_changed = true;
            }
        }
        for report in self.decoder.drain_boss_reports() {
            self.field_bosses.note_report(&report);
            self.field_bosses_changed = true;
        }
        self.combat.ingest(&self.scratch);
        if let (Some(last), Some(next)) = (self.fight.last_event_at(), self.scratch.first().map(|e| e.timestamp)) {
            if next - last > FIGHT_GAP {
                self.finish_fight();
            }
        }
        self.fight.ingest(&self.scratch);
        self.apply_local_id_changes();
        for (summon, owner) in self.decoder.entities.drain_resolved_owners() {
            self.combat.reattribute(summon, owner);
            self.fight.reattribute(summon, owner);
        }
    }

    fn apply_local_id_changes(&mut self) {
        for (from, to) in self.decoder.entities.drain_local_id_changes() {
            self.combat.merge_ids(from, to);
            self.fight.merge_ids(from, to);
            self.buffs.merge_ids(from, to);
        }
    }

    /// Ends the current fight, saving it when it was against a boss.
    fn finish_fight(&mut self) {
        let entities = &self.decoder.entities;
        let mut on_bosses: HashMap<i32, i64> = HashMap::new();
        for e in self.fight.events().iter().filter(|e| !e.is_heal && entities.boss_npc_id_of(e.target).is_some()) {
            *on_bosses.entry(e.target).or_default() += e.amount;
        }
        let boss = on_bosses.into_iter().max_by_key(|b| b.1).and_then(|(id, _)| bosses::find(entities.boss_npc_id_of(id)?));
        if let (Some(boss), Some(start), Some(end)) = (boss, self.fight.started_at, self.fight.last_event_at()) {
            if end - start >= MIN_BOSS_FIGHT {
                if let Some(mut summary) = sessions::summarize(self.decoder.current_zone_id, start, end, &self.fight, entities, &self.buffs) {
                    summary.boss = Some(boss.name.clone());
                    self.finished.push(summary);
                }
            }
        }
        self.fight.clear();
    }

    pub fn finish_session(&mut self, end: Micros) {
        self.finish_fight();
        let start = self.session_start.unwrap_or(end);
        let zone = self.zone_of_session;
        if let Some(summary) = sessions::summarize(zone, start, end, &self.combat, &self.decoder.entities, &self.buffs) {
            self.finished.push(summary);
        }
        self.combat.clear();
        self.buffs.forget_before(end);
        self.session_start = Some(end);
        self.zone_of_session = self.decoder.current_zone_id;
    }

    pub fn drain_finished_sessions(&mut self) -> Vec<SessionSummary> {
        std::mem::take(&mut self.finished)
    }

    pub fn drain_zone_changes(&mut self) -> Vec<ZoneChange> {
        std::mem::take(&mut self.zone_changes)
    }

    pub fn last_events(&self) -> &[DamageEvent] {
        &self.scratch
    }
}
