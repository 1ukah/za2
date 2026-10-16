use std::sync::Arc;

use crate::combat::CombatLog;
use crate::segment::SECOND;

/// A pause in combat this long ends a fight; the next hit starts a new one.
pub const FIGHT_GAP: crate::segment::Micros = 10 * SECOND;
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

        for change in self.decoder.drain_zone_changes() {
            if self.reset_on_zone_change {
                self.finish_session(change.timestamp);
            }
            self.zone_changes.push(change);
        }
        self.combat.ingest(&self.scratch);
        if let (Some(last), Some(next)) = (self.fight.last_event_at(), self.scratch.first().map(|e| e.timestamp)) {
            if next - last > FIGHT_GAP {
                self.fight.clear();
            }
        }
        self.fight.ingest(&self.scratch);
        for (from, to) in self.decoder.entities.drain_local_id_changes() {
            self.combat.merge_ids(from, to);
            self.fight.merge_ids(from, to);
        }
        for (summon, owner) in self.decoder.entities.drain_resolved_owners() {
            self.combat.reattribute(summon, owner);
            self.fight.reattribute(summon, owner);
        }
    }

    pub fn finish_session(&mut self, end: Micros) {
        let start = self.session_start.unwrap_or(end);
        let zone = self.zone_of_session;
        if let Some(summary) = sessions::summarize(zone, start, end, &self.combat, &self.decoder.entities) {
            self.finished.push(summary);
        }
        self.combat.clear();
        self.fight.clear();
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
