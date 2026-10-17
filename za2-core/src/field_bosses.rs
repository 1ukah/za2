//! What is known of the field bosses: from the map reports (sent when the player opens the map) and from kills seen.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::catalog::field_bosses;
use crate::decoder::FieldBossReport;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum BossState {
    /// Up since (unix ms; 0: unknown).
    Up { since_ms: i64 },
    /// Dead; back at (unix ms; 0: the game gave no time).
    Down { back_ms: i64 },
    /// Killed in sight at (unix ms), its return not known until the map is opened again.
    Killed { at_ms: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BossStatus {
    pub state: BossState,
    /// When the game told it (unix ms).
    pub seen_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldBossBoard {
    /// By the boss's world-map id.
    pub bosses: HashMap<u32, BossStatus>,
}

impl FieldBossBoard {
    pub fn note_report(&mut self, report: &FieldBossReport) {
        let seen_ms = report.timestamp / 1000;
        for b in &report.bosses {
            let state = if b.up { BossState::Up { since_ms: b.time_ms } } else { BossState::Down { back_ms: b.time_ms } };
            self.bosses.insert(b.boss, BossStatus { state, seen_ms });
        }
    }

    /// A field boss died in sight. Returns whether `npc` is one.
    pub fn note_kill(&mut self, npc: i32, at_ms: i64) -> bool {
        let Some(boss) = field_bosses::by_npc(npc) else { return false };
        self.bosses.insert(boss.world_id, BossStatus { state: BossState::Killed { at_ms }, seen_ms: at_ms });
        true
    }

    /// The state at `now_ms`: a boss whose return time passed is up.
    pub fn state_at(&self, world_id: u32, now_ms: i64) -> Option<BossState> {
        let status = self.bosses.get(&world_id)?;
        Some(match status.state {
            BossState::Down { back_ms } if back_ms > 0 && back_ms <= now_ms => BossState::Up { since_ms: back_ms },
            other => other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::FieldBossSighting;

    #[test]
    fn a_boss_is_back_at_its_time() {
        let mut board = FieldBossBoard::default();
        let report = FieldBossReport {
            timestamp: 1_000_000_000,
            map_id: 1110,
            bosses: vec![FieldBossSighting { boss: 111017, up: false, time_ms: 2_000_000 }, FieldBossSighting { boss: 111001, up: true, time_ms: 900_000 }],
        };
        board.note_report(&report);
        assert_eq!(board.state_at(111017, 1_500_000), Some(BossState::Down { back_ms: 2_000_000 }));
        assert_eq!(board.state_at(111017, 2_500_000), Some(BossState::Up { since_ms: 2_000_000 }));
        assert_eq!(board.state_at(111001, 1_500_000), Some(BossState::Up { since_ms: 900_000 }));
        assert_eq!(board.state_at(111002, 1_500_000), None);
    }
}
