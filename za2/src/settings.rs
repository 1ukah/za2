use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One meter overlay. metric: 0 damage, 1 healing. range: 0 current fight, 1 total. scope: 0 all players, 1 group, 2 only me.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub id: u32,
    pub enabled: bool,
    pub metric: u8,
    pub range: u8,
    pub scope: u8,
    /// Window size (logical pixels) the user gave the overlay; None: the default size.
    pub size: Option<(f32, f32)>,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        OverlayConfig { id: 1, enabled: true, metric: 0, range: 0, scope: 0, size: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub own_character_name: String,
    pub reset_on_zone_change: bool,
    pub scope: i32,
    pub gather_items: BTreeSet<String>,
    /// Pets to farm: all monsters that give them are marked.
    pub pets: BTreeSet<i32>,
    /// Monsters marked on their own (npc ids).
    pub monsters: BTreeSet<i32>,
    pub show_live_mobs: bool,
    pub show_cubes: bool,
    pub map_radius_metres: f64,
    pub overlay_radius_metres: f64,
    pub last_map: Option<String>,
    pub last_x: Option<f32>,
    pub last_y: Option<f32>,
    /// Shows the meter overlays (each also has its own switch).
    pub meter_overlay: bool,
    pub overlays: Vec<OverlayConfig>,
    /// The meter table shows the current fight (0) or the total (1).
    pub meter_range: u8,
    pub map_overlay: bool,
    /// Window size (logical pixels) of the map overlay; None: the default size.
    pub map_overlay_size: Option<(f32, f32)>,
    pub overlays_locked: bool,
    pub overlay_opacity: f32,
    pub capture_device: Option<String>,
    pub cube_sound_enabled: bool,
    pub cube_sound: String,
    pub cube_sound_volume: u32,
    /// Pets shown in the pet overlay.
    pub pinned_pets: BTreeSet<i32>,
    pub pet_overlay: bool,
    /// Window size (logical pixels) of the pet overlay; None: the default size.
    pub pet_overlay_size: Option<(f32, f32)>,
    /// The pet overlay also shows the pet of the monster the player fights.
    pub pet_overlay_target: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            own_character_name: String::new(),
            reset_on_zone_change: true,
            scope: 0,
            gather_items: BTreeSet::new(),
            pets: BTreeSet::new(),
            monsters: BTreeSet::new(),
            show_live_mobs: true,
            show_cubes: true,
            map_radius_metres: 250.0,
            overlay_radius_metres: 150.0,
            last_map: None,
            last_x: None,
            last_y: None,
            meter_overlay: false,
            overlays: vec![OverlayConfig::default()],
            meter_range: 0,
            map_overlay: false,
            map_overlay_size: None,
            overlays_locked: false,
            overlay_opacity: 0.85,
            capture_device: None,
            cube_sound_enabled: false,
            cube_sound: "ping".into(),
            cube_sound_volume: 70,
            pinned_pets: BTreeSet::new(),
            pet_overlay: false,
            pet_overlay_size: None,
            pet_overlay_target: true,
        }
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("za2")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("za2")
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}
