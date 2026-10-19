//! Reads the own character's position and the NPCs around it from the game's memory and writes them to stdout
//! (see `za2_core::memory_stream`). Read-only: it never writes to the game or stops it.
//!
//! Reading another process needs `cap_sys_ptrace` when `kernel.yama.ptrace_scope` is 1:
//! `sudo setcap cap_sys_ptrace=eip za2-memory`.

mod process;
mod reader;
mod unreal;

use std::io::{BufRead, BufWriter, Write};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use za2_core::memory_stream::{MemoryCommand, MemoryMessage};

use reader::Reader;
use unreal::Unreal;

const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const WAIT_INTERVAL: Duration = Duration::from_secs(3);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
const EPERM: i32 = 1;

struct Output {
    out: BufWriter<std::io::Stdout>,
}

impl Output {
    fn send(&mut self, message: &MemoryMessage) {
        let ok = writeln!(self.out, "{}", message.to_line()).is_ok();
        if !ok {
            std::process::exit(0);
        }
    }

    fn status(&mut self, text: &str) {
        self.send(&MemoryMessage::Status(text.into()));
        self.flush();
    }

    fn flush(&mut self) {
        if self.out.flush().is_err() {
            std::process::exit(0);
        }
    }
}

fn commands() -> Receiver<MemoryCommand> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Some(command) = MemoryCommand::parse(&line) {
                if tx.send(command).is_err() {
                    return;
                }
            }
        }
        // stdin closes when za2 ends, even when it crashes or is killed.
        if std::env::var_os("ZA2_EXIT_WITH_PARENT").is_some() {
            std::process::exit(0);
        }
    });
    rx
}

fn read_game(reader: &mut Reader, out: &mut Output, commands: &Receiver<MemoryCommand>) {
    let mut described = false;
    loop {
        let started = Instant::now();
        while let Ok(command) = commands.try_recv() {
            match command {
                MemoryCommand::IdOffset(offset) => reader.id_offset = offset,
                MemoryCommand::Probe { actor, entity } => {
                    for offset in reader.probe(actor, entity) {
                        out.send(&MemoryMessage::Found { actor, entity, offset });
                    }
                }
            }
        }
        if !reader.unreal.process.alive() {
            out.status("the game exited");
            return;
        }
        let frame = reader.frame();
        match frame.own {
            Some(own) => {
                if !described {
                    described = true;
                    out.status(&format!("reading your character in {}: {}", own.world.as_deref().unwrap_or("an unnamed world"), reader.describe()));
                }
                out.send(&MemoryMessage::SelfFix(own));
            }
            None => out.send(&MemoryMessage::NoSelf),
        }
        for npc in frame.npcs {
            out.send(&MemoryMessage::Npc(npc));
        }
        out.send(&MemoryMessage::Frame);
        out.flush();
        if let Some(rest) = FRAME_INTERVAL.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

fn main() {
    let mut out = Output { out: BufWriter::new(std::io::stdout()) };
    let commands = commands();
    let mut waiting = false;
    loop {
        let Some(game) = process::find_game() else {
            if !waiting {
                waiting = true;
                out.status("waiting for the game");
            }
            std::thread::sleep(WAIT_INTERVAL);
            continue;
        };
        waiting = false;
        if let Err(e) = game.process.read_vec(game.base, 2) {
            if e.raw_os_error() == Some(EPERM) {
                out.status("no permission to read the game's memory: run  sudo setcap cap_sys_ptrace=eip <path to za2-memory>");
                std::process::exit(3);
            }
            out.status(&format!("cannot read the game (pid {}): {e}", game.process.pid));
            std::thread::sleep(RETRY_INTERVAL);
            continue;
        }
        let started = Instant::now();
        let reader = Unreal::discover(game.process, game.base).and_then(Reader::new);
        match reader {
            Ok(mut reader) => {
                out.status(&format!("reading the game (pid {}), engine data found in {:.1} s", game.process.pid, started.elapsed().as_secs_f64()));
                read_game(&mut reader, &mut out, &commands);
            }
            Err(e) => {
                out.status(&format!("game found (pid {}), but {e}; retrying", game.process.pid));
                std::thread::sleep(RETRY_INTERVAL);
            }
        }
    }
}
