use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

const COMMANDS: [&str; 7] = ["show", "reset", "toggle-overlay", "toggle-map-overlay", "toggle-lock", "map-zoom-in", "map-zoom-out"];

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("za2.sock")
}

pub fn send(command: &str) -> Result<(), String> {
    if !COMMANDS.contains(&command) {
        return Err(format!("unknown command {command}; known: {}", COMMANDS.join(", ")));
    }
    let mut stream = UnixStream::connect(socket_path()).map_err(|e| format!("no running meter ({e})"))?;
    stream.write_all(format!("{command}\n").as_bytes()).map_err(|e| e.to_string())
}

pub fn listen(handler: impl Fn(String) + Send + 'static) {
    let path = socket_path();
    if UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(listener) = UnixListener::bind(&path) else { return };
    std::thread::Builder::new()
        .name("za2-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() {
                    let command = line.trim().to_string();
                    if COMMANDS.contains(&command.as_str()) {
                        handler(command);
                    }
                }
            }
        })
        .ok();
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
