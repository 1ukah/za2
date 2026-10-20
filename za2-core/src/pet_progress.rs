//! The local player's pets: level and progress of each. The game's memory has the whole collection at any time
//! (`za2-memory`, the client's `VehicleSystem`); the packets have it only in the login frame and then announce each
//! change (a kill of a pet's monster, a new level, an unlocked pet).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Level 0 is a pet not unlocked yet, 3 the top.
pub const MAX_LEVEL: u8 = 3;

/// Kills a pet needs to leave `level`: 5 unlock it, 25 more give level 2, 75 more level 3 (105 in all). Seen in the
/// login list (pets not unlocked hold 1 to 4) and in the game's memory (24/25 became level 2 with 0/75).
pub fn needed(level: u8) -> u32 {
    match level {
        0 => 5,
        1 => 25,
        2 => 75,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PetState {
    pub level: u8,
    pub progress: u32,
}

impl PetState {
    pub fn is_max(&self) -> bool {
        self.level >= MAX_LEVEL
    }

    /// Kills until the next level.
    pub fn left(&self) -> u32 {
        needed(self.level).saturating_sub(self.progress)
    }

    /// Kills until the top level.
    pub fn left_to_max(&self) -> u32 {
        self.left() + (self.level + 1..MAX_LEVEL).map(needed).sum::<u32>()
    }

    /// Adds kills, moving up a level each time one is full.
    fn gain(&mut self, kills: u32) {
        self.progress += kills;
        while !self.is_max() && self.progress >= needed(self.level) {
            self.progress -= needed(self.level);
            self.level += 1;
        }
        if self.is_max() {
            self.progress = 0;
        }
    }
}

/// What the packets say about the pets.
#[derive(Clone, Debug, PartialEq)]
pub enum PetEvent {
    /// The login list: every pet with a level or progress.
    List(BTreeMap<i32, PetState>),
    /// Kills counted for a pet.
    Progress { pet: i32, gained: u32 },
    /// A pet reached a level.
    Level { pet: i32, level: u8 },
    /// A pet was unlocked.
    Added { pet: i32, level: u8 },
}

/// Level and progress of every pet; a pet that is not listed has neither.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PetBook {
    pets: BTreeMap<i32, PetState>,
}

impl PetBook {
    pub fn from_states(pets: impl IntoIterator<Item = (i32, PetState)>) -> PetBook {
        PetBook { pets: pets.into_iter().filter(|(_, s)| *s != PetState::default()).collect() }
    }

    pub fn get(&self, pet: i32) -> PetState {
        self.pets.get(&pet).copied().unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.pets.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (i32, PetState)> + '_ {
        self.pets.iter().map(|(&id, &s)| (id, s))
    }

    pub fn unlocked(&self) -> usize {
        self.pets.values().filter(|s| s.level > 0).count()
    }

    pub fn maxed(&self) -> usize {
        self.pets.values().filter(|s| s.is_max()).count()
    }

    /// Applies what the packets announced. A level or unlock never moves a pet down: the progress already counted it.
    pub fn apply(&mut self, event: &PetEvent) {
        match event {
            PetEvent::List(pets) => *self = PetBook::from_states(pets.iter().map(|(&id, &s)| (id, s))),
            PetEvent::Progress { pet, gained } => self.pets.entry(*pet).or_default().gain(*gained),
            PetEvent::Level { pet, level } | PetEvent::Added { pet, level } => {
                let state = self.pets.entry(*pet).or_default();
                if *level > state.level {
                    *state = PetState { level: (*level).min(MAX_LEVEL), progress: 0 };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kills_fill_the_levels_up_to_105() {
        let mut s = PetState::default();
        assert_eq!((s.left(), s.left_to_max()), (5, 105));
        s.gain(4);
        assert_eq!((s.level, s.progress, s.left()), (0, 4, 1));
        s.gain(1);
        assert_eq!((s.level, s.progress, s.left(), s.left_to_max()), (1, 0, 25, 100));
        s.gain(24);
        s.gain(1);
        assert_eq!((s.level, s.progress, s.left_to_max()), (2, 0, 75));
        s.gain(80);
        assert_eq!((s.level, s.progress, s.left(), s.left_to_max()), (3, 0, 0, 0));
        assert!(s.is_max());
    }

    #[test]
    fn a_level_frame_after_the_kill_that_reached_it_changes_nothing() {
        let mut book = PetBook::from_states([(1063, PetState { level: 1, progress: 24 })]);
        book.apply(&PetEvent::Progress { pet: 1063, gained: 1 });
        book.apply(&PetEvent::Level { pet: 1063, level: 2 });
        assert_eq!(book.get(1063), PetState { level: 2, progress: 0 });
        book.apply(&PetEvent::Progress { pet: 1063, gained: 1 });
        book.apply(&PetEvent::Level { pet: 1063, level: 2 });
        assert_eq!(book.get(1063), PetState { level: 2, progress: 1 }, "a repeated level frame keeps the progress");
    }

    #[test]
    fn the_login_list_replaces_everything() {
        let mut book = PetBook::from_states([(1001, PetState { level: 2, progress: 3 })]);
        book.apply(&PetEvent::List(BTreeMap::from([(1045, PetState { level: 0, progress: 3 }), (1002, PetState::default())])));
        assert_eq!(book.iter().collect::<Vec<_>>(), vec![(1045, PetState { level: 0, progress: 3 })]);
        assert_eq!(book.get(1001), PetState::default());
        assert_eq!((book.unlocked(), book.maxed()), (0, 0));
    }
}
