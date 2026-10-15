use std::path::PathBuf;

use za2_core::capture_stream::{read_message, CaptureMessage};
use za2_core::catalog::zones;
use za2_core::engine::Engine;
use za2_core::position::PositionTracker;
use za2_core::protocol::Protocol;
use za2_core::recording;
use za2_core::segment::{Micros, TcpSegment};

struct Options {
    input: Option<PathBuf>,
    from_capture: bool,
    record: Option<PathBuf>,
    own_name: Option<String>,
    verbose: bool,
    seconds: Option<u64>,
}

fn usage() -> ! {
    eprintln!("usage: za2-replay <recording.jsonl> [--name OWN_CHARACTER] [--verbose]");
    eprintln!("       za2-capture --ports 13328 | za2-replay --from-capture [--record out.jsonl] [--seconds N] [--verbose]");
    std::process::exit(2);
}

fn parse() -> Options {
    let mut o = Options { input: None, from_capture: false, record: None, own_name: None, verbose: false, seconds: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--from-capture" => o.from_capture = true,
            "--record" => o.record = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--name" => o.own_name = Some(args.next().unwrap_or_else(|| usage())),
            "--verbose" => o.verbose = true,
            "--seconds" => o.seconds = Some(args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
            "-h" | "--help" => usage(),
            other if o.input.is_none() && !other.starts_with("--") => o.input = Some(other.into()),
            _ => usage(),
        }
    }
    if o.input.is_none() && !o.from_capture {
        usage();
    }
    o
}

struct Run {
    engine: Engine,
    tracker: PositionTracker,
    writer: Option<recording::Writer>,
    verbose: bool,
    last_report: Micros,
    last_position: Option<(f32, f32)>,
}

impl Run {
    fn feed(&mut self, segment: &TcpSegment) {
        if let Some(w) = &mut self.writer {
            let _ = w.write(segment);
        }
        self.engine.ingest(segment);
        for change in self.engine.drain_zone_changes() {
            let name = zones::name_of(change.zone_id, "en").unwrap_or("?");
            println!("{} zone {} ({name}){}", recording::format_time(change.timestamp), change.zone_id, if change.restart { " restart" } else { "" });
        }
        if self.verbose {
            for e in self.engine.last_events() {
                let who = self.engine.decoder.entities.name_for(e.source).unwrap_or_else(|| format!("#{}", e.source));
                println!(
                    "{} {} {} -> {} {} {}{}{}",
                    recording::format_time(e.timestamp),
                    who,
                    e.skill,
                    e.target,
                    e.amount,
                    if e.is_heal { "heal" } else { "dmg" },
                    if e.is_critical { " crit" } else { "" },
                    if e.is_tick { " tick" } else { "" }
                );
            }
        }
        if segment.timestamp - self.last_report >= 1_000_000 {
            self.last_report = segment.timestamp;
            let tracked = self.tracker.update(&self.engine.decoder.entities, segment.timestamp, None);
            let at = (tracked.x, tracked.y);
            if self.last_position != Some(at) && tracked.map.is_some() {
                self.last_position = Some(at);
                println!(
                    "{} position {} {:.0},{:.0} on {} (age {:.1}s)",
                    recording::format_time(segment.timestamp),
                    tracked.state.label(),
                    tracked.x,
                    tracked.y,
                    tracked.map.map_or("?", |m| m.title.as_str()),
                    tracked.age as f64 / 1e6
                );
            }
        }
    }

    fn summary(&mut self) {
        let e = &self.engine;
        let r = &e.reassembler;
        let s = &e.decoder.stats;
        println!("--");
        println!("segments {}  frames {}  bundles {} (failed {})  gaps {}  retransmissions {}  resyncs {}", e.segments, r.frames, s.bundles, s.bundle_failures, r.gaps, r.retransmissions, r.desyncs);
        println!("damage/heal events {}  unknown opcodes {}  short frames {}  no-damage frames {}", e.events, s.unknown_opcodes, s.skipped_short_frames, s.no_damage_frames);
        if self.verbose {
            let mut counts: Vec<_> = s.opcode_counts.iter().collect();
            counts.sort_by(|a, b| b.1.cmp(a.1));
            let line: Vec<String> = counts.iter().take(40).map(|(op, n)| format!("{op:04x}:{n}")).collect();
            println!("opcodes {}", line.join(" "));
        }
        let entities = &e.decoder.entities;
        if let Some(c) = entities.local_character() {
            println!("local character {} (combat id {}, level {}, server {})", c.name, c.combat_id, c.level, c.server_id);
        } else if let Some(id) = entities.local_player_id() {
            println!("local player id {id} (inferred)");
        }
        let mut rows: Vec<_> = e.combat.damage_by_source().iter().filter(|(id, _)| entities.is_known_player(**id)).collect();
        rows.sort_by(|a, b| b.1.sum.cmp(&a.1.sum));
        for (id, stats) in rows.iter().take(20) {
            let name = entities.name_for(**id).unwrap_or_else(|| format!("#{id}"));
            let class = entities.class_of(**id).unwrap_or("?");
            let heal = e.combat.healing_by_source().get(id).copied().unwrap_or(0);
            println!(
                "  {:<20} {:<13} damage {:>12}  dps {:>9}  hits {:>5}  crit {:>5.1}%  heal {:>10}",
                name,
                class,
                stats.sum,
                stats.rate().map_or("-".into(), |r| format!("{r:.0}")),
                stats.hits,
                if stats.hits > 0 { 100.0 * stats.crits as f64 / stats.hits as f64 } else { 0.0 },
                heal
            );
        }
        let now = e.last_segment_at.unwrap_or(0);
        let tracked = self.tracker.update(entities, now, None);
        println!(
            "map {}  position {} {:.0},{:.0}",
            tracked.map.map_or("unknown", |m| m.title.as_str()),
            tracked.state.label(),
            tracked.x,
            tracked.y
        );
        if let Some(w) = &mut self.writer {
            let _ = w.flush();
            println!("recorded {} segments", w.count);
        }
    }
}

fn main() {
    let o = parse();
    let protocol = Protocol::load();
    if !protocol.calibrated {
        eprintln!("za2-replay: opcodes.json not found or not calibrated (set ZA2_ASSETS)");
        std::process::exit(1);
    }
    let mut engine = Engine::new(protocol);
    engine.decoder.entities.set_configured_local_name(o.own_name.as_deref());
    let writer = o.record.as_ref().map(|p| recording::Writer::create(p).unwrap_or_else(|e| {
        eprintln!("za2-replay: cannot create {}: {e}", p.display());
        std::process::exit(1);
    }));
    let mut run = Run { engine, tracker: PositionTracker::new(), writer, verbose: o.verbose, last_report: 0, last_position: None };

    if o.from_capture {
        let stdin = std::io::stdin();
        let mut reader = std::io::BufReader::new(stdin.lock());
        let started = std::time::Instant::now();
        loop {
            match read_message(&mut reader) {
                Ok(CaptureMessage::Segment(seg)) => run.feed(&seg),
                Ok(CaptureMessage::Status(text)) => eprintln!("capture: {text}"),
                Ok(CaptureMessage::Stats { received, dropped, if_dropped, segments }) => {
                    if run.verbose {
                        eprintln!("capture: received {received} dropped {dropped} if_dropped {if_dropped} segments {segments}");
                    }
                    if let Some(w) = &mut run.writer {
                        let _ = w.flush();
                    }
                }
                Err(_) => break,
            }
            if o.seconds.is_some_and(|limit| started.elapsed().as_secs() >= limit) {
                break;
            }
        }
    } else {
        let path = o.input.unwrap();
        match recording::read(&path) {
            Ok(segments) => {
                for seg in segments {
                    run.feed(&seg);
                }
            }
            Err(e) => {
                eprintln!("za2-replay: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    run.summary();
}
