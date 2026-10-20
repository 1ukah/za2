//! The link to `za2-memory`: own position from the game's memory, and NPC positions that replace the packet
//! positions of the same entities. NPC actors carry the server's entity id at an offset that is not a reflected
//! property; it is checked against the packets all the time and searched for again when it stops matching.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use za2_core::entities::LiveMob;
use za2_core::memory_stream::{MemoryCommand, MemoryMessage, NpcActor, SelfFix};
use za2_core::segment::{Micros, SECOND};

/// The id offset of the build of 2026-10-15; the helper starts with it too.
const DEFAULT_ID_OFFSET: usize = 0x26f0;
const RESTART_DELAY: Duration = Duration::from_secs(10);
const MISSING_RETRY: Duration = Duration::from_secs(60);
const NPCS_FRESH: Micros = SECOND;
/// An actor this close to a monster the packets placed is taken as that monster when checking ids ...
const SAME_PLACE: f32 = 100.0;
/// ... this close when searching for the id offset ...
const PROBE_PLACE: f32 = 30.0;
/// ... and a memory position replaces a packet position of the same id only when they are this close.
const MERGE_RANGE: f32 = 2000.0;
const CHECK_WINDOW: usize = 200;
const CHECK_EVIDENCE: usize = 20;
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const PROBES_PER_ROUND: usize = 8;
const PROBE_VOTES: usize = 4;

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn nearest(mobs: &[LiveMob], x: f32, y: f32) -> Option<(&LiveMob, f32)> {
    mobs.iter().map(|m| (m, distance((m.x, m.y), (x, y)))).min_by(|a, b| a.1.total_cmp(&b.1))
}

#[derive(Default)]
struct IdCheck {
    offset: usize,
    results: VecDeque<bool>,
    probes: HashMap<u64, (i32, Instant)>,
    votes: HashMap<usize, HashSet<u64>>,
    last_round: Option<Instant>,
}

impl IdCheck {
    fn new() -> IdCheck {
        IdCheck { offset: DEFAULT_ID_OFFSET, ..Default::default() }
    }

    fn match_rate(&self) -> Option<f64> {
        (self.results.len() >= CHECK_EVIDENCE).then(|| self.results.iter().filter(|r| **r).count() as f64 / self.results.len() as f64)
    }

    /// Compares the ids of actors standing where the packets put a monster. Returns a command for the helper.
    fn check(&mut self, npcs: &[NpcActor], mobs: &[LiveMob]) -> Vec<MemoryCommand> {
        if self.offset == 0 {
            return self.probe_round(npcs, mobs);
        }
        for actor in npcs {
            if let Some((mob, d)) = nearest(mobs, actor.x, actor.y) {
                if d <= SAME_PLACE {
                    self.results.push_back(actor.entity == mob.entity);
                    if self.results.len() > CHECK_WINDOW {
                        self.results.pop_front();
                    }
                }
            }
        }
        if self.match_rate().is_some_and(|r| r < 0.5) {
            self.offset = 0;
            self.results.clear();
            return vec![MemoryCommand::IdOffset(0)];
        }
        Vec::new()
    }

    fn probe_round(&mut self, npcs: &[NpcActor], mobs: &[LiveMob]) -> Vec<MemoryCommand> {
        if self.last_round.is_some_and(|at| at.elapsed() < PROBE_INTERVAL) {
            return Vec::new();
        }
        self.last_round = Some(Instant::now());
        let mut out = Vec::new();
        for actor in npcs {
            if out.len() >= PROBES_PER_ROUND || self.probes.contains_key(&actor.actor) {
                continue;
            }
            let close: Vec<&LiveMob> = mobs.iter().filter(|m| distance((m.x, m.y), (actor.x, actor.y)) <= PROBE_PLACE).collect();
            if let [mob] = close[..] {
                self.probes.insert(actor.actor, (mob.entity, Instant::now()));
                out.push(MemoryCommand::Probe { actor: actor.actor, entity: mob.entity });
            }
        }
        out
    }

    fn found(&mut self, actor: u64, entity: i32, offset: usize) -> Vec<MemoryCommand> {
        if self.offset != 0 || self.probes.get(&actor).map(|p| p.0) != Some(entity) {
            return Vec::new();
        }
        self.votes.entry(offset).or_default().insert(actor);
        let answered = self.probes.values().filter(|p| p.1.elapsed() >= Duration::from_secs(1)).count().max(1);
        let best = self.votes.iter().map(|(o, actors)| (*o, actors.len())).max_by_key(|v| (v.1, std::cmp::Reverse(v.0)));
        match best {
            Some((offset, votes)) if votes >= PROBE_VOTES && votes * 10 >= answered * 8 => {
                *self = IdCheck { offset, ..Default::default() };
                vec![MemoryCommand::IdOffset(offset)]
            }
            _ => Vec::new(),
        }
    }
}

pub enum Event {
    Message(MemoryMessage),
    Exited,
}

pub struct MemoryLink {
    helper: Option<PathBuf>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    restart_at: Option<Instant>,
    pub status: String,
    own: Option<(SelfFix, Micros)>,
    npcs: Vec<NpcActor>,
    building: Vec<NpcActor>,
    npcs_at: Micros,
    ids: IdCheck,
}

impl MemoryLink {
    pub fn new(helper: Option<PathBuf>) -> MemoryLink {
        MemoryLink {
            status: if helper.is_some() { "starting".into() } else { "off".into() },
            restart_at: helper.as_ref().map(|_| Instant::now()),
            helper,
            child: None,
            stdin: None,
            own: None,
            npcs: Vec::new(),
            building: Vec::new(),
            npcs_at: 0,
            ids: IdCheck::new(),
        }
    }

    /// Starts the helper when it is due. `deliver` hands its messages to the worker and returns false once the
    /// worker is gone.
    pub fn tick(&mut self, deliver: impl Fn(Event) -> bool + Send + Clone + 'static) {
        let (Some(helper), Some(at)) = (&self.helper, self.restart_at) else { return };
        if Instant::now() < at {
            return;
        }
        self.restart_at = None;
        let spawned = Command::new(helper).env("ZA2_EXIT_WITH_PARENT", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("cannot start {}: {e}", helper.display());
                self.restart_at = Some(Instant::now() + MISSING_RETRY);
                return;
            }
        };
        let stdout = child.stdout.take().unwrap();
        self.stdin = child.stdin.take();
        self.child = Some(child);
        self.ids = IdCheck::new();
        self.status = "helper started".into();
        std::thread::Builder::new()
            .name("za2-memory-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(message) = MemoryMessage::parse(&line) {
                        if !deliver(Event::Message(message)) {
                            return;
                        }
                    }
                }
                deliver(Event::Exited);
            })
            .expect("cannot start the memory reader thread");
    }

    fn send(&mut self, commands: Vec<MemoryCommand>) {
        if let Some(stdin) = &mut self.stdin {
            for c in commands {
                if writeln!(stdin, "{}", c.to_line()).is_err() {
                    break;
                }
            }
            let _ = stdin.flush();
        }
    }

    pub fn handle(&mut self, event: Event, now: Micros, mobs: &[LiveMob]) {
        match event {
            Event::Message(MemoryMessage::Status(text)) => self.status = text,
            Event::Message(MemoryMessage::SelfFix(fix)) => self.own = Some((fix, now)),
            Event::Message(MemoryMessage::NoSelf) => self.own = None,
            Event::Message(MemoryMessage::Npc(npc)) => self.building.push(npc),
            Event::Message(MemoryMessage::Frame) => {
                self.npcs = std::mem::take(&mut self.building);
                self.npcs_at = now;
                let commands = self.ids.check(&self.npcs, mobs);
                self.send(commands);
            }
            Event::Message(MemoryMessage::Found { actor, entity, offset }) => {
                let commands = self.ids.found(actor, entity, offset);
                self.send(commands);
            }
            // the worker keeps the pets
            Event::Message(MemoryMessage::Pets(_) | MemoryMessage::NoPets) => {}
            Event::Exited => {
                let code = self.child.take().and_then(|mut c| c.wait().ok()).and_then(|s| s.code());
                self.stdin = None;
                self.own = None;
                self.npcs.clear();
                if code == Some(3) {
                    self.status = format!("{} (map position from packets only)", self.status);
                } else {
                    self.status = format!("helper stopped (exit {code:?}), restarting");
                    self.restart_at = Some(Instant::now() + RESTART_DELAY);
                }
            }
        }
    }

    /// Asks the helper to look for the pets again (the list it reads belongs to another character).
    pub fn search_pets(&mut self) {
        self.send(vec![MemoryCommand::PetSearch]);
    }

    pub fn own(&self) -> Option<(&SelfFix, Micros)> {
        self.own.as_ref().map(|(f, at)| (f, *at))
    }

    pub fn live(&self, now: Micros) -> bool {
        self.own.as_ref().is_some_and(|o| now - o.1 < 2 * SECOND)
    }

    /// Moves the monsters the packets know to where the game draws them.
    pub fn merge(&self, mobs: &mut [LiveMob], now: Micros) {
        if now - self.npcs_at > NPCS_FRESH || self.ids.offset == 0 {
            return;
        }
        let by_entity: HashMap<i32, &NpcActor> = self.npcs.iter().filter(|a| a.entity != 0).map(|a| (a.entity, a)).collect();
        for mob in mobs {
            if let Some(a) = by_entity.get(&mob.entity) {
                if distance((a.x, a.y), (mob.x, mob.y)) <= MERGE_RANGE {
                    (mob.x, mob.y, mob.z) = (a.x, a.y, a.z);
                }
            }
        }
    }

    pub fn describe(&self) -> String {
        let ids = match (self.ids.offset, self.ids.match_rate()) {
            (0, _) => format!("NPC ids: searching ({} probed)", self.ids.probes.len()),
            (o, Some(r)) => format!("NPC ids at +{o:#x}, {:.0}% match the packets", r * 100.0),
            (o, None) => format!("NPC ids at +{o:#x}, checking"),
        };
        format!("{}  |  {} NPCs  |  {ids}", self.status, self.npcs.len())
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mob(entity: i32, x: f32, y: f32) -> LiveMob {
        LiveMob { entity, npc: 1, x, y, z: 0.0 }
    }

    fn actor(actor: u64, entity: i32, x: f32, y: f32) -> NpcActor {
        NpcActor { actor, entity, x, y, z: 500.0 }
    }

    #[test]
    fn wrong_ids_start_a_search_that_finds_the_offset() {
        let mobs: Vec<LiveMob> = (0..10).map(|i| mob(100 + i, i as f32 * 1000.0, 0.0)).collect();
        let wrong: Vec<NpcActor> = (0..10).map(|i| actor(0x1000 + i as u64, 7, i as f32 * 1000.0 + 5.0, 0.0)).collect();
        let mut check = IdCheck::new();
        assert!(check.check(&wrong, &mobs).is_empty(), "not enough evidence yet");
        assert_eq!(check.check(&wrong, &mobs), vec![MemoryCommand::IdOffset(0)]);

        let probes = check.check(&wrong, &mobs);
        assert_eq!(probes.len(), PROBES_PER_ROUND);
        assert!(check.check(&wrong, &mobs).is_empty(), "one round per interval");
        let mut adopted = Vec::new();
        for p in probes {
            if !adopted.is_empty() {
                assert!(check.probes.is_empty(), "adopting the offset ends the search");
                break;
            }
            let MemoryCommand::Probe { actor, entity } = p else { panic!() };
            check.probes.get_mut(&actor).unwrap().1 -= Duration::from_secs(2);
            assert!(check.found(actor, entity + 1, 0x30).is_empty(), "an answer for another id does not count");
            adopted.extend(check.found(actor, entity, 0x9999 + (actor as usize % 2) * 8));
            adopted.extend(check.found(actor, entity, 0x2700));
        }
        assert_eq!(adopted, vec![MemoryCommand::IdOffset(0x2700)]);
        assert_eq!(check.offset, 0x2700);
    }

    #[test]
    fn matching_ids_move_the_packet_monsters() {
        let mut link = MemoryLink::new(None);
        let mut mobs = vec![mob(5, 1000.0, 1000.0), mob(6, 5000.0, 5000.0), mob(7, 9000.0, 9000.0)];
        let now = 10 * SECOND;
        for a in [actor(1, 5, 1100.0, 1050.0), actor(2, 6, 9000.0, 9000.0), actor(3, 0, 9000.0, 9000.0)] {
            link.handle(Event::Message(MemoryMessage::Npc(a)), now, &mobs);
        }
        link.handle(Event::Message(MemoryMessage::Frame), now, &mobs);
        link.merge(&mut mobs, now);
        assert_eq!((mobs[0].x, mobs[0].y, mobs[0].z), (1100.0, 1050.0, 500.0));
        assert_eq!((mobs[1].x, mobs[1].y), (5000.0, 5000.0), "an id far from its packet place is not trusted");
        assert_eq!((mobs[2].x, mobs[2].y), (9000.0, 9000.0));
        let mut later = mobs.clone();
        link.merge(&mut later, now + 2 * SECOND);
        assert_eq!(later, mobs, "an old NPC list is not used");
    }
}
