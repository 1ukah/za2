use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use za2_core::capture_stream::{read_message, CaptureMessage};
use za2_core::catalog::zones;
use za2_core::combat::SkillUsage;
use za2_core::engine::Engine;
use za2_core::combat::CombatLog;
use za2_core::entities::{EntityDirectory, EnvObject, LiveMob};
use za2_core::position::{PositionTracker, TrackedPosition};
use za2_core::protocol::Protocol;
use za2_core::recording;
use za2_core::segment::{now_micros, Micros, TcpSegment, SECOND};
use za2_core::sessions::{self, SessionSummary};
use za2_core::ttk::TtkEstimator;

use crate::memory::{self, MemoryLink};

const QUEUE_CAPACITY: usize = 16_384;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// While the memory reader delivers positions, the map follows them at the UI's refresh rate.
const PUBLISH_INTERVAL_MEMORY: Duration = Duration::from_millis(100);
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// The client pings every ten seconds: a reading older than this means the connection is gone.
const PING_STALE: Micros = 30 * SECOND;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaptureState {
    Starting,
    Waiting,
    Connected,
    Error,
    Replay,
    Finished,
}

#[derive(Clone, Debug, Default)]
pub struct Counters {
    pub segments: u64,
    pub frames: u64,
    pub events: u64,
    pub resyncs: u64,
    pub gaps: u64,
    pub retransmissions: u64,
    pub bundles: u64,
    pub bundle_failures: u64,
    pub unknown_opcodes: u64,
    pub short_frames: u64,
    pub helper_received: u32,
    pub helper_dropped: u32,
    pub helper_if_dropped: u32,
    pub queue_dropped: u64,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: i32,
    pub name: String,
    pub class: Option<&'static str>,
    /// Gear score and combat power from the party roster: party members only.
    pub gear_score: Option<u32>,
    pub combat_power: Option<u64>,
    pub damage: i64,
    pub dps: Option<f64>,
    pub heal: i64,
    pub hits: u32,
    pub crits: u32,
    pub local: bool,
    pub estimated: bool,
    pub in_group: bool,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub version: u64,
    pub state: CaptureState,
    pub status: String,
    pub counters: Counters,
    pub zone_id: i32,
    pub zone_name: String,
    /// Players of everything since the zone was entered or the meter reset.
    pub rows: Vec<Row>,
    pub encounter_seconds: f64,
    /// Players of the current fight (see `engine::FIGHT_GAP`).
    pub fight_rows: Vec<Row>,
    pub fight_seconds: f64,
    pub local_name: Option<String>,
    pub learned_name: Option<String>,
    pub faction: Option<&'static str>,
    pub group_known: bool,
    pub position: Option<TrackedPosition>,
    pub live_mobs: Vec<LiveMob>,
    pub live_env: Vec<EnvObject>,
    /// Skill breakdown of each selected player (the meter's and the overlays' selections).
    pub skills: HashMap<i32, Vec<SkillUsage>>,
    pub target: Option<(String, i64, i64)>,
    /// Round trip to the game server (ms).
    pub ping: Option<i64>,
    /// Estimated time until the local player's target dies (seconds).
    pub time_to_kill: Option<f64>,
    pub character: Option<String>,
    pub sessions: Arc<Vec<SessionSummary>>,
    pub recording: Option<(PathBuf, u64)>,
    pub memory_status: String,
    pub now: Micros,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            version: 0,
            state: CaptureState::Starting,
            status: "starting".into(),
            counters: Counters::default(),
            zone_id: 0,
            zone_name: String::new(),
            rows: Vec::new(),
            encounter_seconds: 0.0,
            fight_rows: Vec::new(),
            fight_seconds: 0.0,
            local_name: None,
            learned_name: None,
            faction: None,
            group_known: false,
            position: None,
            live_mobs: Vec::new(),
            live_env: Vec::new(),
            skills: HashMap::new(),
            target: None,
            ping: None,
            time_to_kill: None,
            character: None,
            sessions: Arc::new(Vec::new()),
            recording: None,
            memory_status: String::new(),
            now: 0,
        }
    }
}

pub enum WorkerCommand {
    Reset,
    OwnName(String),
    ResetOnZone(bool),
    /// The players whose skills the snapshot carries.
    SelectSources(Vec<i32>),
    StartRecording(PathBuf),
    StopRecording,
    RestoreMap(Option<String>, Option<(f32, f32)>),
}

pub enum Source {
    Live { helper: PathBuf, device: Option<String>, memory_helper: Option<PathBuf> },
    Replay { path: PathBuf, speed: f64 },
}

enum Input {
    Message(CaptureMessage),
    Memory(memory::Event),
    HelperExited(String),
    ReplayDone,
}

pub struct Shared {
    pub snapshot: Mutex<Snapshot>,
    pub version: AtomicU64,
}

pub fn start(source: Source, own_name: String, reset_on_zone: bool) -> (Arc<Shared>, SyncSender<WorkerCommand>) {
    let shared = Arc::new(Shared { snapshot: Mutex::new(Snapshot::default()), version: AtomicU64::new(0) });
    let (command_tx, command_rx) = sync_channel::<WorkerCommand>(256);
    let shared_worker = shared.clone();
    std::thread::Builder::new()
        .name("za2-decoder".into())
        .spawn(move || run(source, own_name, reset_on_zone, shared_worker, command_rx))
        .expect("cannot start decoder thread");
    (shared, command_tx)
}

fn spawn_helper(helper: &PathBuf, ports: &[u16], device: &Option<String>, tx: SyncSender<Input>, dropped: Arc<AtomicU64>) -> std::io::Result<Child> {
    let mut command = Command::new(helper);
    command.arg("--ports").arg(ports.iter().map(u16::to_string).collect::<Vec<_>>().join(","));
    if let Some(d) = device {
        command.arg("--device").arg(d);
    }
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_tx = tx.clone();
    let last_error = Arc::new(Mutex::new(String::new()));
    let last_error_reader = last_error.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            *last_error_reader.lock().unwrap() = line.clone();
            let _ = err_tx.try_send(Input::Message(CaptureMessage::Status(line)));
        }
    });
    std::thread::Builder::new().name("za2-capture-reader".into()).spawn(move || {
        let mut reader = BufReader::with_capacity(256 * 1024, stdout);
        loop {
            match read_message(&mut reader) {
                Ok(message) => {
                    let blocking = !matches!(message, CaptureMessage::Segment(_));
                    let result = if blocking { tx.send(Input::Message(message)).map_err(|_| ()) } else {
                        match tx.try_send(Input::Message(message)) {
                            Ok(()) => Ok(()),
                            Err(TrySendError::Full(_)) => {
                                dropped.fetch_add(1, Ordering::Relaxed);
                                Ok(())
                            }
                            Err(TrySendError::Disconnected(_)) => Err(()),
                        }
                    };
                    if result.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let text = last_error.lock().unwrap().clone();
                    let _ = tx.send(Input::HelperExited(text));
                    return;
                }
            }
        }
    })?;
    Ok(child)
}

fn spawn_replay(path: PathBuf, speed: f64, tx: SyncSender<Input>) {
    std::thread::spawn(move || {
        let segments = match recording::read(&path) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(Input::Message(CaptureMessage::Status(format!("cannot read {}: {e}", path.display()))));
                let _ = tx.send(Input::ReplayDone);
                return;
            }
        };
        let started = Instant::now();
        let mut first: Option<Micros> = None;
        for segment in segments {
            if speed > 0.0 {
                let base = *first.get_or_insert(segment.timestamp);
                let due = Duration::from_micros(((segment.timestamp - base).max(0) as f64 / speed) as u64);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
            if tx.send(Input::Message(CaptureMessage::Segment(segment))).is_err() {
                return;
            }
        }
        let _ = tx.send(Input::ReplayDone);
    });
}

struct Worker {
    sessions: Vec<SessionSummary>,
    sessions_changed: bool,
    engine: Engine,
    tracker: PositionTracker,
    ttk: TtkEstimator,
    snapshot: Snapshot,
    selected: Vec<i32>,
    replay: bool,
    recorder: Option<(PathBuf, recording::Writer)>,
    queue_dropped: Arc<AtomicU64>,
    dirty: bool,
    tracked_at: Micros,
    memory: MemoryLink,
}

impl Worker {
    fn now(&self) -> Micros {
        if self.replay {
            self.engine.last_segment_at.unwrap_or_else(now_micros)
        } else {
            now_micros()
        }
    }

    fn collect_sessions(&mut self) {
        let finished = self.engine.drain_finished_sessions();
        if finished.is_empty() {
            return;
        }
        self.sessions.extend(finished);
        let now = self.now();
        sessions::prune(&mut self.sessions, now);
        if !self.replay {
            save_sessions(&self.sessions);
        }
        self.sessions_changed = true;
    }

    fn segment(&mut self, segment: TcpSegment) {
        if let Some((_, writer)) = &mut self.recorder {
            let _ = writer.write(&segment);
        }
        self.engine.ingest(&segment);
        if self.snapshot.state != CaptureState::Replay && self.snapshot.state != CaptureState::Connected && segment.from_server {
            self.snapshot.state = CaptureState::Connected;
            self.snapshot.status = format!("game traffic from {}", segment.source);
        }
        for change in self.engine.drain_zone_changes() {
            self.snapshot.zone_id = change.zone_id;
            self.tracker.forget();
        }
        self.collect_sessions();
        if segment.from_server && segment.timestamp - self.tracked_at >= 1_000_000 {
            self.tracked_at = segment.timestamp;
            self.snapshot.position = Some(self.tracker.update(&self.engine.decoder.entities, segment.timestamp, self.memory.own()));
        }
        self.dirty = true;
    }

    fn publish(&mut self, shared: &Shared) {
        if self.sessions_changed {
            self.sessions_changed = false;
            self.snapshot.sessions = Arc::new(self.sessions.clone());
        }
        let now = self.now();
        let engine = &mut self.engine;
        let entities = &engine.decoder.entities;
        let local = entities.local_player_id();
        let party = entities.party_names();
        let rows = player_rows(&engine.combat, entities);
        let fight_rows = player_rows(&engine.fight, entities);
        let s = &mut self.snapshot;
        s.rows = rows;
        s.encounter_seconds = log_seconds(&engine.combat);
        s.fight_rows = fight_rows;
        s.fight_seconds = log_seconds(&engine.fight);
        s.local_name = local.and_then(|id| entities.name_for(id));
        s.learned_name = entities.learned_local_name().map(str::to_string);
        s.character = entities.local_character().map(|c| {
            format!(
                "{}, {} level {}, server {}",
                c.name,
                za2_core::catalog::skills::class_from_code(c.class_code).unwrap_or("unknown class"),
                c.level,
                c.server_id
            )
        });
        s.target = entities.local_hit_target().filter(|t| now - t.1 < 60_000_000).and_then(|(id, _)| {
            let name = entities
                .boss_npc_id_of(id)
                .and_then(za2_core::catalog::bosses::find)
                .map(|b| b.name.clone())
                .or_else(|| entities.monster_name_for(id, "en").map(str::to_string))
                .unwrap_or_else(|| format!("#{id}"));
            let hp = entities.hit_points.latest(id)?.hp;
            Some((name, hp, entities.hit_points.max_of(id).unwrap_or(hp)))
        });
        s.ping = engine.ping.current().filter(|(_, at)| now - at < PING_STALE).map(|(ms, _)| ms);
        s.time_to_kill = time_to_kill(&mut self.ttk, engine, now);
        s.faction = entities.local_character().and_then(|c| match c.server_id / 1000 {
            1 => Some("Elyos"),
            2 => Some("Asmodian"),
            _ if c.class_code % 4 == 2 => Some("Elyos"),
            _ => None,
        });
        let own = entities.local_character().map(|c| c.name.clone());
        s.group_known = party.iter().any(|n| Some(n) != own.as_ref());
        s.zone_id = engine.decoder.current_zone_id;
        s.zone_name = if s.zone_id > 0 { zones::name_of(s.zone_id, "en").map(str::to_string).unwrap_or_else(|| format!("zone {}", s.zone_id)) } else { String::new() };
        s.position = Some(self.tracker.update(&engine.decoder.entities, now, self.memory.own()));
        s.live_mobs = engine.decoder.entities.live_mobs().copied().collect();
        self.memory.merge(&mut s.live_mobs, now);
        s.memory_status = self.memory.describe();
        s.live_env = engine.decoder.entities.live_env(now);
        s.skills = self.selected.iter().map(|&id| (id, engine.combat.skill_breakdown(id, false))).collect();
        let r = &engine.reassembler;
        let d = &engine.decoder.stats;
        s.counters.segments = engine.segments;
        s.counters.frames = r.frames;
        s.counters.events = engine.events;
        s.counters.resyncs = r.desyncs;
        s.counters.gaps = r.gaps;
        s.counters.retransmissions = r.retransmissions;
        s.counters.bundles = d.bundles;
        s.counters.bundle_failures = d.bundle_failures;
        s.counters.unknown_opcodes = d.unknown_opcodes;
        s.counters.short_frames = d.skipped_short_frames;
        s.counters.queue_dropped = self.queue_dropped.load(Ordering::Relaxed);
        s.recording = self.recorder.as_ref().map(|(p, w)| (p.clone(), w.count));
        s.now = now;
        s.version += 1;
        *shared.snapshot.lock().unwrap() = s.clone();
        shared.version.store(s.version, Ordering::Release);
        self.dirty = false;
    }

    fn command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Reset => {
                let now = self.now();
                self.engine.finish_session(now);
                self.collect_sessions();
            }
            WorkerCommand::OwnName(name) => self.engine.decoder.entities.set_configured_local_name(Some(&name)),
            WorkerCommand::ResetOnZone(on) => self.engine.reset_on_zone_change = on,
            WorkerCommand::SelectSources(ids) => {
                self.selected = ids;
                self.dirty = true;
            }
            WorkerCommand::StartRecording(path) => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match recording::Writer::create(&path) {
                    Ok(w) => self.recorder = Some((path, w)),
                    Err(e) => self.snapshot.status = format!("cannot record to {}: {e}", path.display()),
                }
            }
            WorkerCommand::StopRecording => {
                if let Some((_, mut w)) = self.recorder.take() {
                    let _ = w.flush();
                }
            }
            WorkerCommand::RestoreMap(key, last) => self.tracker.restore(key.as_deref(), last),
        }
        self.dirty = true;
    }
}

fn run(source: Source, own_name: String, reset_on_zone: bool, shared: Arc<Shared>, commands: Receiver<WorkerCommand>) {
    let protocol = Protocol::load();
    let ports = protocol.server_ports.clone();
    let calibrated = protocol.calibrated;
    let mut engine = Engine::new(protocol);
    engine.reset_on_zone_change = reset_on_zone;
    if matches!(source, Source::Live { .. }) {
        engine.ping.set_perf_clock(za2_core::ping::perf_clock());
    }
    engine.decoder.entities.set_configured_local_name(Some(&own_name));
    let queue_dropped = Arc::new(AtomicU64::new(0));
    let mut worker = Worker {
        engine,
        tracker: PositionTracker::new(),
        ttk: TtkEstimator::new(),
        snapshot: Snapshot::default(),
        selected: Vec::new(),
        replay: matches!(source, Source::Replay { .. }),
        recorder: None,
        queue_dropped: queue_dropped.clone(),
        dirty: true,
        tracked_at: 0,
        memory: MemoryLink::new(match &source {
            Source::Live { memory_helper, .. } => memory_helper.clone(),
            Source::Replay { .. } => None,
        }),
        sessions: load_sessions(),
        sessions_changed: true,
    };

    let (tx, rx) = sync_channel::<Input>(QUEUE_CAPACITY);
    let mut child: Option<Child> = None;
    let mut restart_at: Option<Instant> = None;
    if !calibrated {
        worker.snapshot.state = CaptureState::Error;
        worker.snapshot.status = format!("protocol description not found under {}", za2_core::assets::root().display());
    } else {
        match &source {
            Source::Live { .. } => restart_at = Some(Instant::now()),
            Source::Replay { path, speed } => {
                worker.snapshot.state = CaptureState::Replay;
                worker.snapshot.status = format!("replaying {}", path.display());
                spawn_replay(path.clone(), *speed, tx.clone());
            }
        }
    }

    let mut last_publish = Instant::now() - PUBLISH_INTERVAL;
    loop {
        if let Some(at) = restart_at {
            if Instant::now() >= at {
                restart_at = None;
                if let Source::Live { helper, device, .. } = &source {
                    match spawn_helper(helper, &ports, device, tx.clone(), queue_dropped.clone()) {
                        Ok(c) => {
                            child = Some(c);
                            worker.snapshot.state = CaptureState::Waiting;
                            worker.snapshot.status = "capture helper started".into();
                        }
                        Err(e) => {
                            worker.snapshot.state = CaptureState::Error;
                            worker.snapshot.status = format!("cannot start {}: {e}", helper.display());
                            restart_at = Some(Instant::now() + RESTART_DELAY * 2);
                        }
                    }
                    worker.dirty = true;
                }
            }
        }

        let memory_tx = tx.clone();
        worker.memory.tick(move |event| memory_tx.send(Input::Memory(event)).is_ok());

        loop {
            match commands.try_recv() {
                Ok(c) => worker.command(c),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(mut c) = child.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    worker.memory.stop();
                    return;
                }
            }
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(input) => {
                handle(&mut worker, input, &mut child, &mut restart_at);
                let deadline = Instant::now() + Duration::from_millis(20);
                while Instant::now() < deadline {
                    match rx.try_recv() {
                        Ok(input) => handle(&mut worker, input, &mut child, &mut restart_at),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {}
        }

        let interval = if worker.memory.live(worker.now()) { PUBLISH_INTERVAL_MEMORY } else { PUBLISH_INTERVAL };
        let position_tick = last_publish.elapsed() >= Duration::from_millis(500);
        if (worker.dirty && last_publish.elapsed() >= interval) || position_tick {
            last_publish = Instant::now();
            worker.publish(&shared);
        }
    }
}

fn handle(worker: &mut Worker, input: Input, child: &mut Option<Child>, restart_at: &mut Option<Instant>) {
    match input {
        Input::Message(CaptureMessage::Segment(segment)) => worker.segment(segment),
        Input::Memory(event) => {
            let now = worker.now();
            let frame_done = matches!(event, memory::Event::Message(za2_core::memory_stream::MemoryMessage::Frame));
            let mobs: Vec<LiveMob> = if frame_done { worker.engine.decoder.entities.live_mobs().copied().collect() } else { Vec::new() };
            worker.memory.handle(event, now, &mobs);
            worker.dirty = true;
        }
        Input::Message(CaptureMessage::Status(text)) => {
            if text.contains("waiting") && worker.snapshot.state != CaptureState::Replay {
                worker.snapshot.state = CaptureState::Waiting;
            }
            if text.starts_with("capturing") && worker.snapshot.state == CaptureState::Starting {
                worker.snapshot.state = CaptureState::Waiting;
            }
            worker.snapshot.status = text;
            worker.dirty = true;
        }
        Input::Message(CaptureMessage::Stats { received, dropped, if_dropped, .. }) => {
            worker.snapshot.counters.helper_received = received;
            worker.snapshot.counters.helper_dropped = dropped;
            worker.snapshot.counters.helper_if_dropped = if_dropped;
            if let Some((_, w)) = &mut worker.recorder {
                let _ = w.flush();
            }
            worker.dirty = true;
        }
        Input::HelperExited(last) => {
            let code = child.as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
            *child = None;
            worker.snapshot.state = CaptureState::Error;
            worker.snapshot.status = match code {
                Some(3) => "capture helper has no capture permission: run  sudo setcap cap_net_raw,cap_net_admin=eip <path to za2-capture>".into(),
                _ if !last.is_empty() => format!("capture helper stopped: {last}"),
                _ => format!("capture helper stopped (exit {code:?}), restarting"),
            };
            if code != Some(3) {
                *restart_at = Some(Instant::now() + RESTART_DELAY);
            }
            worker.dirty = true;
        }
        Input::ReplayDone => {
            worker.snapshot.state = CaptureState::Finished;
            worker.snapshot.status = "replay finished".into();
            worker.dirty = true;
        }
    }
}

/// Seconds until the local player's target dies, from the predicted length of the current fight.
fn time_to_kill(ttk: &mut TtkEstimator, engine: &Engine, now: Micros) -> Option<f64> {
    let entities = &engine.decoder.entities;
    let target = entities.local_hit_target().filter(|t| now - t.1 < 60 * SECOND).map(|t| t.0);
    let (Some(id), Some(start)) = (target, engine.fight.started_at) else {
        ttk.reset(None);
        return None;
    };
    let sample = entities.hit_points.latest(id)?;
    let max = entities.hit_points.max_of(id)?;
    let total_ms = ttk.update(id, (sample.at - start) as f64 / 1000.0, sample.hp, max)?;
    let elapsed_ms = (now - start) as f64 / 1000.0;
    Some(((total_ms - elapsed_ms) / 1000.0).max(0.0))
}

fn log_seconds(log: &CombatLog) -> f64 {
    match (log.started_at, log.last_event_at()) {
        (Some(a), Some(b)) => (b - a) as f64 / 1e6,
        _ => 0.0,
    }
}

/// The players of a combat log with their damage and healing, most damage first.
fn player_rows(log: &CombatLog, entities: &EntityDirectory) -> Vec<Row> {
    let local = entities.local_player_id();
    let party = entities.party_names();
    let healing = log.healing_by_source();
    let mut ids: HashSet<i32> = log.damage_by_source().keys().copied().collect();
    ids.extend(healing.keys().copied());
    let mut rows: Vec<Row> = Vec::new();
    for id in ids {
        if !entities.is_known_player(id) {
            continue;
        }
        let stats = log.damage_by_source().get(&id).cloned().unwrap_or_default();
        let name = entities.name_for(id).unwrap_or_else(|| format!("Player #{id}"));
        let party_stats = entities.party_stats(&name);
        rows.push(Row {
            id,
            in_group: Some(id) == local || party.contains(&name) || entities.local_character().is_some_and(|c| c.name == name),
            name,
            class: entities.class_of(id),
            gear_score: party_stats.map(|p| p.gear_score),
            combat_power: party_stats.and_then(|p| p.combat_power),
            damage: stats.sum,
            dps: stats.rate(),
            heal: healing.get(&id).copied().unwrap_or(0),
            hits: stats.hits,
            crits: stats.crits,
            local: Some(id) == local,
            estimated: stats.estimated_sum * 2 > stats.sum,
        });
    }
    rows.sort_by(|a, b| b.damage.cmp(&a.damage).then(b.heal.cmp(&a.heal)));
    rows
}

fn sessions_path() -> PathBuf {
    crate::settings::data_dir().join("sessions.json")
}

fn load_sessions() -> Vec<SessionSummary> {
    let mut list: Vec<SessionSummary> = std::fs::read_to_string(sessions_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    sessions::prune(&mut list, now_micros());
    list
}

fn save_sessions(list: &[SessionSummary]) {
    let path = sessions_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(list) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}
