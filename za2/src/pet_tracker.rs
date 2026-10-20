//! The pets as the meter shows them. The game's memory has them all the time (`za2-memory`); without it the packets
//! keep them: the login list, then every kill, on top of what was saved for the character the last time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use za2_core::pet_progress::{PetBook, PetEvent, PetState};

/// The helper repeats the list every 5 seconds: older than this, the memory is gone.
const MEMORY_STALE: Duration = Duration::from_secs(15);
const SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// After a login the memory may still hold the previous character: at most this often the helper is asked to look again.
const SEARCH_INTERVAL: Duration = Duration::from_secs(10);
/// The memory and the login list may differ by kills that were on their way; more than this many pets is another character.
const SAME_CHARACTER_SLACK: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PetSource {
    #[default]
    None,
    /// What was saved for the character; kills since are counted when the packets see them.
    Saved,
    Packets,
    Memory,
}

pub struct PetTracker {
    book: PetBook,
    source: PetSource,
    memory_at: Option<Instant>,
    /// A login list arrived that the memory did not agree with: packets count until the memory reads the same.
    memory_doubted: bool,
    searched_at: Option<Instant>,
    character: Option<String>,
    /// The character the list belongs to; None after a login list, which comes just before the login's character record.
    owner: Option<String>,
    saved: HashMap<String, PetBook>,
    path: Option<PathBuf>,
    dirty: bool,
    saved_at: Instant,
    pub version: u64,
}

fn differing(a: &PetBook, b: &PetBook) -> usize {
    let mut ids: Vec<i32> = a.iter().chain(b.iter()).map(|(id, _)| id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter().filter(|&id| a.get(id) != b.get(id)).count()
}

impl PetTracker {
    /// `path`: where the pets of each character are kept between runs (None: nowhere, for a replay).
    pub fn new(path: Option<PathBuf>) -> PetTracker {
        let saved = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        PetTracker {
            book: PetBook::default(),
            source: PetSource::None,
            memory_at: None,
            memory_doubted: false,
            searched_at: None,
            character: None,
            owner: None,
            saved,
            path,
            dirty: false,
            saved_at: Instant::now(),
            version: 0,
        }
    }

    pub fn book(&self) -> &PetBook {
        &self.book
    }

    pub fn source(&self) -> PetSource {
        self.source
    }

    fn memory_live(&self) -> bool {
        !self.memory_doubted && self.memory_at.is_some_and(|at| at.elapsed() < MEMORY_STALE)
    }

    fn set(&mut self, book: PetBook, source: PetSource) {
        if book != self.book || source != self.source {
            self.book = book;
            self.source = source;
            self.version += 1;
            self.dirty = true;
        }
    }

    /// The list read from memory. Returns true when the helper should look for another list (it shows another character).
    pub fn memory(&mut self, pets: Vec<(i32, PetState)>) -> bool {
        let book = PetBook::from_states(pets);
        self.memory_at = Some(Instant::now());
        if self.memory_doubted {
            if differing(&book, &self.book) > SAME_CHARACTER_SLACK {
                return self.search_due();
            }
            self.memory_doubted = false;
        }
        self.set(book, PetSource::Memory);
        false
    }

    pub fn memory_gone(&mut self) {
        self.memory_at = None;
        if self.source == PetSource::Memory {
            self.source = PetSource::Packets;
            self.version += 1;
        }
    }

    /// What the packets announced. Returns true when the helper should look for another list.
    pub fn packet(&mut self, event: &PetEvent) -> bool {
        if let PetEvent::List(_) = event {
            self.save(true);
            self.owner = None;
            let mut book = PetBook::default();
            book.apply(event);
            // a login: the memory has to show this list (another character's would differ in many pets)
            let doubt = self.memory_live() && differing(&book, &self.book) > SAME_CHARACTER_SLACK;
            self.set(book, PetSource::Packets);
            if doubt {
                self.memory_doubted = true;
                return self.search_due();
            }
            return false;
        }
        if self.memory_live() {
            // the memory shows the kill on its next read; counting it here too would count it twice
            return false;
        }
        let mut book = self.book.clone();
        book.apply(event);
        let source = if self.source == PetSource::None { PetSource::Packets } else { self.source };
        self.set(book, source);
        false
    }

    fn search_due(&mut self) -> bool {
        if self.searched_at.is_some_and(|at| at.elapsed() < SEARCH_INTERVAL) {
            return false;
        }
        self.searched_at = Some(Instant::now());
        true
    }

    /// The own character (name and server), once known. It takes over a list that has no owner yet; another
    /// character's list makes way for what was saved for this one, until better arrives.
    pub fn set_character(&mut self, key: Option<String>) {
        if key.is_none() || key == self.character {
            return;
        }
        self.save(true);
        self.character = key.clone();
        match &self.owner {
            None if self.source != PetSource::None => self.owner = key,
            Some(owner) if Some(owner) == key.as_ref() => {}
            _ => {
                self.owner = key;
                let saved = self.character.as_ref().and_then(|k| self.saved.get(k)).cloned();
                match saved {
                    Some(book) => self.set(book, PetSource::Saved),
                    None => self.set(PetBook::default(), PetSource::None),
                }
                self.dirty = false;
            }
        }
    }

    /// Writes the pets of the character when they changed (`now`: without waiting).
    pub fn save(&mut self, now: bool) {
        if !self.dirty || (!now && self.saved_at.elapsed() < SAVE_INTERVAL) {
            return;
        }
        let (Some(path), Some(key)) = (&self.path, &self.character) else { return };
        if self.owner.as_ref() != Some(key) || self.source == PetSource::None || self.book.is_empty() {
            return;
        }
        self.dirty = false;
        self.saved_at = Instant::now();
        self.saved.insert(key.clone(), self.book.clone());
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&self.saved) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn state(level: u8, progress: u32) -> PetState {
        PetState { level, progress }
    }

    fn list(pets: &[(i32, PetState)]) -> PetEvent {
        PetEvent::List(pets.iter().copied().collect::<BTreeMap<_, _>>())
    }

    #[test]
    fn kills_count_from_the_saved_list_without_memory() {
        let dir = std::env::temp_dir().join(format!("za2-pets-test-{}", std::process::id()));
        let path = dir.join("pets.json");
        let mut t = PetTracker::new(Some(path.clone()));
        t.packet(&list(&[(1063, state(1, 12))]));
        t.set_character(Some("Kat@2403".into()));
        t.packet(&PetEvent::Progress { pet: 1063, gained: 1 });
        t.save(true);

        let mut later = PetTracker::new(Some(path));
        later.set_character(Some("Kat@2403".into()));
        assert_eq!((later.book().get(1063), later.source()), (state(1, 13), PetSource::Saved));
        later.packet(&PetEvent::Progress { pet: 1063, gained: 1 });
        assert_eq!((later.book().get(1063), later.source()), (state(1, 14), PetSource::Saved));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_login_list_belongs_to_the_character_record_after_it() {
        let dir = std::env::temp_dir().join(format!("za2-pets-login-test-{}", std::process::id()));
        let mut t = PetTracker::new(Some(dir.join("pets.json")));
        t.packet(&list(&[(1001, state(1, 1))]));
        t.set_character(Some("A@1".into()));
        t.packet(&PetEvent::Progress { pet: 1001, gained: 1 });
        // a relog: B's list comes before B's character record
        t.packet(&list(&[(1002, state(2, 5))]));
        t.set_character(Some("B@1".into()));
        t.save(true);
        assert_eq!(t.saved.get("A@1").map(|b| b.get(1001)), Some(state(1, 2)));
        assert_eq!(t.saved.get("B@1").map(|b| b.get(1002)), Some(state(2, 5)));
        // back to A without its list (za2 started late): A's saved pets
        t.set_character(Some("A@1".into()));
        assert_eq!((t.book().get(1001), t.source()), (state(1, 2), PetSource::Saved));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn memory_wins_and_kills_are_not_counted_twice() {
        let mut t = PetTracker::new(None);
        assert!(!t.memory(vec![(1063, state(1, 12))]));
        t.packet(&PetEvent::Progress { pet: 1063, gained: 1 });
        assert_eq!((t.book().get(1063), t.source()), (state(1, 12), PetSource::Memory), "the memory shows it on its next read");
        t.memory(vec![(1063, state(1, 13))]);
        assert_eq!(t.book().get(1063), state(1, 13));
        t.memory_gone();
        t.packet(&PetEvent::Progress { pet: 1063, gained: 1 });
        assert_eq!((t.book().get(1063), t.source()), (state(1, 14), PetSource::Packets));
    }

    #[test]
    fn a_login_of_another_character_doubts_the_memory_until_it_agrees() {
        let mut t = PetTracker::new(None);
        let old: Vec<(i32, PetState)> = (1001..1010).map(|id| (id, state(1, 3))).collect();
        t.memory(old.clone());
        let new: Vec<(i32, PetState)> = (1020..1025).map(|id| (id, state(2, 1))).collect();
        assert!(t.packet(&list(&new)), "ask the helper to look again");
        assert_eq!(t.source(), PetSource::Packets);
        assert!(!t.memory(old), "the old list again, but the helper was asked a moment ago");
        assert_eq!(t.book().get(1020), state(2, 1), "the packets keep the new character's list");
        t.packet(&PetEvent::Progress { pet: 1020, gained: 1 });
        assert_eq!(t.book().get(1020), state(2, 2));
        let mut agreeing = new.clone();
        agreeing[0].1 = state(2, 2);
        t.memory(agreeing);
        assert_eq!((t.book().get(1020), t.source()), (state(2, 2), PetSource::Memory));
    }
}
