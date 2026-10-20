//! The local player's pets from the game's memory. The client keeps them in its `VehicleSystem` (one per character
//! session; "vehicle" is the client's word for a pet): an array of shared pointers to one record per pet, `pet id u32,
//! level u32, progress u32, unlocked u32`. Neither the array nor the record is a reflected property, so the array is
//! found by its contents and every read is checked; a read that does not look like the pet list starts a new search.
//! Build of 2026-10-19: the array at +0x50, 209 pets, checked against the login list and live kills.

use std::time::{Duration, Instant};

use za2_core::pet_progress::{PetState, MAX_LEVEL};

use crate::unreal::{plausible_pointer, Unreal};

const CLASS: &str = "/Script/Aion2.VehicleSystem";
/// How much of the system object is searched for the array.
const OBJECT_BYTES: usize = 0x400;
/// One array element: the record and the shared pointer's reference counter.
const ELEMENT: usize = 16;
const RECORD: usize = 16;
const MIN_PETS: usize = 50;
const MAX_PETS: usize = 4096;
const SEARCH_RETRY: Duration = Duration::from_secs(5);

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// A pet record, or None when the bytes are not one: an unlocked pet has a level, a locked one has none.
fn record(data: &[u8]) -> Option<(i32, PetState)> {
    let (pet, level, progress, unlocked) = (u32_at(data, 0), u32_at(data, 4), u32_at(data, 8), u32_at(data, 12));
    let plausible = (1000..=99_999).contains(&pet) && level <= MAX_LEVEL as u32 && progress < 1000 && unlocked == (level > 0) as u32;
    plausible.then_some((pet as i32, PetState { level: level as u8, progress }))
}

#[derive(Clone, Copy)]
struct Found {
    system: u64,
    class: u64,
    array: u64,
}

#[derive(Default)]
pub struct PetReader {
    found: Option<Found>,
    searched_at: Option<Instant>,
    /// Systems the meter said hold another character's pets: skipped by the next search.
    rejected: Vec<u64>,
}

impl PetReader {
    /// Forgets the list, so the next read searches again, preferring another system than the current one.
    pub fn search_again(&mut self) {
        if let Some(f) = self.found.take() {
            self.rejected.push(f.system);
        }
        self.searched_at = None;
    }

    /// Every pet with a level or progress, None when the list cannot be read.
    pub fn read(&mut self, unreal: &mut Unreal) -> Option<Vec<(i32, PetState)>> {
        if let Some(f) = self.found {
            match (unreal.class_of(f.system) == Some(f.class)).then(|| read_list(unreal, f.system + f.array)).flatten() {
                Some(list) => return Some(list),
                None => self.found = None,
            }
        }
        if self.searched_at.is_some_and(|at| at.elapsed() < SEARCH_RETRY) {
            return None;
        }
        self.searched_at = Some(Instant::now());
        self.found = self.search(unreal);
        let f = self.found?;
        read_list(unreal, f.system + f.array)
    }

    /// The live systems (not the class default), the ones not rejected first, and in each the array that reads as a pet list.
    fn search(&mut self, unreal: &mut Unreal) -> Option<Found> {
        let class = unreal.class(CLASS)?;
        let objects = unreal.all_objects();
        let classes = unreal.process.read_many(&objects.iter().map(|o| o + unreal.layout.class).collect::<Vec<_>>(), 8);
        let mut systems: Vec<u64> = objects
            .iter()
            .zip(classes)
            .filter(|(_, c)| c.as_deref().is_some_and(|c| u64_at(c, 0) == class))
            .map(|(&o, _)| o)
            .filter(|&o| !unreal.object_name(o).is_some_and(|n| n.starts_with("Default__")))
            .collect();
        systems.sort_by_key(|s| self.rejected.contains(s));
        if systems.iter().all(|s| self.rejected.contains(s)) {
            self.rejected.clear();
        }
        for system in systems {
            let Ok(data) = unreal.process.read_vec(system, OBJECT_BYTES) else { continue };
            for at in (0x28..OBJECT_BYTES - 16).step_by(8) {
                let (items, num, max) = (u64_at(&data, at), u32_at(&data, at + 8) as usize, u32_at(&data, at + 12) as usize);
                if plausible_pointer(items) && (MIN_PETS..=MAX_PETS).contains(&num) && num <= max && read_list(unreal, system + at as u64).is_some() {
                    return Some(Found { system, class, array: at as u64 });
                }
            }
        }
        None
    }
}

/// The pets of the array whose header (items, num, max) is at `header`, when every element reads as a pet record.
fn read_list(unreal: &Unreal, header: u64) -> Option<Vec<(i32, PetState)>> {
    let p = &unreal.process;
    let head = p.read_vec(header, 16).ok()?;
    let (items, num) = (u64_at(&head, 0), u32_at(&head, 8) as usize);
    if !plausible_pointer(items) || !(MIN_PETS..=MAX_PETS).contains(&num) {
        return None;
    }
    let elements = p.read_vec(items, num * ELEMENT).ok()?;
    let records: Vec<u64> = elements.chunks_exact(ELEMENT).map(|e| u64_at(e, 0)).collect();
    if !records.iter().all(|&r| plausible_pointer(r)) {
        return None;
    }
    let mut pets = Vec::with_capacity(num);
    for data in p.read_many(&records, RECORD) {
        pets.push(record(&data?)?);
    }
    pets.sort_unstable_by_key(|p| p.0);
    let before = pets.len();
    pets.dedup_by_key(|p| p.0);
    (pets.len() == before).then_some(())?;
    pets.retain(|(_, s)| *s != PetState::default());
    Some(pets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(values: [u32; 4]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn records_as_the_game_keeps_them() {
        // records read from the game on 2026-10-19
        assert_eq!(record(&bytes([1063, 1, 12, 1])), Some((1063, PetState { level: 1, progress: 12 })));
        assert_eq!(record(&bytes([1045, 0, 3, 0])), Some((1045, PetState { level: 0, progress: 3 })));
        assert_eq!(record(&bytes([1137, 3, 0, 1])), Some((1137, PetState { level: 3, progress: 0 })));
        assert_eq!(record(&bytes([1045, 0, 3, 1])), None, "unlocked without a level");
        assert_eq!(record(&bytes([1045, 4, 0, 1])), None, "beyond the top level");
        assert_eq!(record(&bytes([0x4d304c48, 1, 1, 1])), None, "a pointer, not a pet id");
    }
}
