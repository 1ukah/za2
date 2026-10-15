use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn set_root(path: PathBuf) {
    let _ = ROOT.set(path);
}

pub fn root() -> &'static Path {
    ROOT.get_or_init(find_root)
}

pub fn path(relative: &str) -> PathBuf {
    root().join(relative)
}

fn find_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZA2_ASSETS") {
        return PathBuf::from(dir);
    }

    let marker = Path::new("protocol").join("opcodes.json");
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            candidates.push(d.join("assets"));
            candidates.push(d.join("share").join("za2").join("assets"));
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets"));

    candidates
        .into_iter()
        .find(|c| c.join(&marker).is_file())
        .unwrap_or_else(|| PathBuf::from("assets"))
}

pub fn read_json(relative: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path(relative)).ok()?;
    serde_json::from_str(&text).ok()
}
