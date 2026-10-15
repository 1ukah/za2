mod control;
mod desktop;
mod map;
mod memory;
mod settings;
mod worker;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use za2_core::catalog::{gather, maps, pets};
use za2_core::catalog::maps::MapInfo;
use za2_core::entities::PositionSource;
use za2_core::position::PositionState;
use slint::winit_030::{winit, WinitWindowAccessor};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use map::{LiveInputs, Marker, StaticLayer, TileLoader, View};
use settings::Settings;
use worker::{CaptureState, Shared, Snapshot, Source, WorkerCommand};

slint::include_modules!();

struct Args {
    replay: Option<PathBuf>,
    speed: f64,
    x11: bool,
    helper: Option<PathBuf>,
    memory_helper: Option<PathBuf>,
    no_memory: bool,
    send: Option<String>,
    tab: i32,
}

fn usage() -> ! {
    eprintln!("usage: za2 [--replay recording.jsonl [--speed N]] [--x11] [--tab meter|map|settings] [--helper PATH] [--memory-helper PATH | --no-memory] [--assets DIR]");
    eprintln!("       za2 --version");
    eprintln!("       za2 --send show|reset|toggle-overlay|toggle-map-overlay|toggle-lock|map-zoom-in|map-zoom-out");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut a = Args { replay: None, speed: 1.0, x11: false, helper: None, memory_helper: None, no_memory: false, send: None, tab: 0 };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--replay" => a.replay = Some(it.next().unwrap_or_else(|| usage()).into()),
            "--speed" => a.speed = it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--x11" => a.x11 = true,
            "--helper" => a.helper = Some(it.next().unwrap_or_else(|| usage()).into()),
            "--memory-helper" => a.memory_helper = Some(it.next().unwrap_or_else(|| usage()).into()),
            "--no-memory" => a.no_memory = true,
            "--assets" => za2_core::assets::set_root(it.next().unwrap_or_else(|| usage()).into()),
            "--tab" => {
                a.tab = match it.next().as_deref() {
                    Some("meter") => 0,
                    Some("map") => 1,
                    Some("settings") => 2,
                    _ => usage(),
                }
            }
            "--send" => a.send = Some(it.next().unwrap_or_else(|| usage())),
            "--version" => {
                println!("za2 {}", env!("ZA2_VERSION"));
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    a
}

fn helper_path(explicit: Option<PathBuf>, variable: &str, name: &str) -> PathBuf {
    if let Some(p) = explicit.or_else(|| std::env::var_os(variable).map(PathBuf::from)) {
        return p;
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        let beside = dir.join(name);
        if beside.is_file() {
            return beside;
        }
    }
    PathBuf::from(name)
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb_u8(r, g, b)
}

fn class_color(class: Option<&str>) -> Color {
    let hex = match class {
        Some("Cleric") => 0xffe27a,
        Some("Chanter") => 0xe0a020,
        Some("Templar") => 0x2f5fd0,
        Some("Gladiator") => 0x5aa9ff,
        Some("Brawler") => 0xe07a5f,
        Some("Assassin") => 0x7ee08f,
        Some("Ranger") => 0x2f9e57,
        Some("Sorcerer") => 0xb98af5,
        Some("Spiritmaster") => 0x8a4fd8,
        _ => 0x9fb3c8,
    };
    Color::from_rgb_u8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn compact(n: i64) -> String {
    let v = n as f64;
    if v.abs() >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if v.abs() >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if v.abs() >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else {
        n.to_string()
    }
}

fn duration_text(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn state_color(state: PositionState) -> Color {
    match state {
        PositionState::Exact => Color::from_rgb_u8(0x68, 0xcb, 0xa4),
        PositionState::Estimated => Color::from_rgb_u8(0xe5, 0xc6, 0x8a),
        PositionState::Stale => Color::from_rgb_u8(0x96, 0x98, 0xa8),
        PositionState::Unknown => Color::from_rgb_u8(0xf1, 0x7b, 0x87),
    }
}

fn state_text(state: PositionState) -> &'static str {
    match state {
        PositionState::Exact => "Exact position",
        PositionState::Estimated => "Estimated from nearby entities",
        PositionState::Stale => "Stale: last known position",
        PositionState::Unknown => "Position unknown",
    }
}

fn faction_of(snapshot: &Snapshot) -> Option<&'static str> {
    snapshot.faction
}

struct MapState {
    manual: Option<&'static MapInfo>,
    center: (f64, f64),
    follow: bool,
    layer: StaticLayer,
    overlay_layer: StaticLayer,
    markers: Vec<Marker>,
    last_saved: Instant,
    last_built: Option<(String, (i64, i64), i64, (i64, i64), u64)>,
    last_overlay: Option<(String, (i64, i64), (i64, i64), u64)>,
}

/// The lists the meter shows. They stay the same objects and change row by row: replacing a whole list rebuilt the row
/// under the mouse several times a second, which made its hover flicker and lost clicks.
struct Models {
    rows: Rc<VecModel<MeterRow>>,
    skills: Rc<VecModel<SkillRow>>,
}

impl Models {
    fn new(app: &App) -> Models {
        let models = Models { rows: Rc::new(VecModel::default()), skills: Rc::new(VecModel::default()) };
        app.set_rows(ModelRc::from(models.rows.clone()));
        app.set_skills(ModelRc::from(models.skills.clone()));
        models
    }
}

/// A meter overlay window and the list it shows.
struct OverlayWindow {
    id: u32,
    window: MeterOverlay,
    rows: Rc<VecModel<OverlayRow>>,
    shown: bool,
}

/// Opens, closes and configures the overlay windows to match the settings.
fn sync_overlays(app: &App, ui: &mut Ui) {
    let configs = ui.settings.overlays.clone();
    ui.overlays.retain(|o| {
        let keep = configs.iter().any(|c| c.id == o.id);
        if !keep {
            let _ = o.window.hide();
        }
        keep
    });
    for cfg in &configs {
        if ui.overlays.iter().any(|o| o.id == cfg.id) {
            continue;
        }
        let window = MeterOverlay::new().expect("cannot create an overlay");
        let rows = Rc::new(VecModel::default());
        window.set_rows(ModelRc::from(rows.clone()));
        window.set_overlay_title(format!("za2 meter overlay {}", cfg.id).into());
        let weak = window.as_weak();
        window.on_drag(move || move_window(&weak.unwrap()));
        let app_weak = app.as_weak();
        window.on_toggle_lock(move || {
            let app = app_weak.unwrap();
            app.set_overlays_locked(true);
            app.invoke_lock_changed(true);
        });
        ui.overlays.push(OverlayWindow { id: cfg.id, window, rows, shown: false });
    }
    let (locked, opacity, all_on) = (ui.settings.overlays_locked, ui.settings.overlay_opacity, ui.settings.meter_overlay);
    for o in &mut ui.overlays {
        let Some(cfg) = configs.iter().find(|c| c.id == o.id) else { continue };
        o.window.set_locked(locked);
        o.window.set_overlay_opacity(opacity);
        let visible = all_on && cfg.enabled;
        if visible && !o.shown {
            o.shown = true;
            let _ = o.window.show();
            let weak = o.window.as_weak();
            slint::Timer::single_shot(Duration::from_millis(300), move || {
                if let Some(w) = weak.upgrade() {
                    let _ = set_click_through(w.window(), locked);
                }
            });
        } else if !visible && o.shown {
            o.shown = false;
            let _ = o.window.hide();
        }
    }
    let list: Vec<OverlayConfigRow> = configs
        .iter()
        .map(|c| OverlayConfigRow { id: c.id as i32, enabled: c.enabled, metric: c.metric as i32, range: c.range as i32, scope: c.scope as i32 })
        .collect();
    app.set_overlay_configs(ModelRc::new(VecModel::from(list)));
}

/// What one overlay shows: damage or healing of the fight or the total, most first.
fn apply_overlay(o: &OverlayWindow, cfg: &settings::OverlayConfig, ui: &Ui) {
    let s = &ui.snapshot;
    let (rows, seconds) = if cfg.range == 0 { (&s.fight_rows, s.fight_seconds) } else { (&s.rows, s.encounter_seconds) };
    let healing = cfg.metric == 1;
    let value = |r: &worker::Row| if healing { r.heal } else { r.damage };
    let mut list: Vec<&worker::Row> = rows
        .iter()
        .filter(|r| match cfg.scope {
            1 => r.in_group || !s.group_known,
            2 => r.local,
            _ => true,
        })
        .filter(|r| value(r) > 0)
        .collect();
    list.sort_by_key(|r| std::cmp::Reverse(value(r)));
    let total: i64 = list.iter().map(|r| value(r)).sum();
    let top = list.first().map_or(1, |r| value(r).max(1));
    let out: Vec<OverlayRow> = list
        .iter()
        .take(10)
        .map(|r| {
            let (gear_score, power) = gear_of(ui, &r.name);
            let rate = if healing { (seconds > 0.0).then(|| r.heal as f64 / seconds) } else { r.dps };
            OverlayRow {
                name: r.name.clone().into(),
                gear_score: gear_score.into(),
                power: power.into(),
                value: compact(value(r)).into(),
                rate: rate.map_or("-".into(), |v| compact(v.round() as i64)).into(),
                share: value(r) as f32 / top as f32,
                share_text: if total > 0 { format!("{:.0}%", 100.0 * value(r) as f64 / total as f64) } else { String::new() }.into(),
                local: r.local,
                color: class_color(r.class),
            }
        })
        .collect();
    sync_model(&o.rows, out);
    let range = if cfg.range == 0 { "Fight" } else { "Total" };
    o.window.set_header(format!("{range} {}  ·  {}", duration_text(seconds), if healing { "Healing" } else { "Damage" }).into());
}

/// Gear score and combat power of a player, as text. No source for them yet: empty.
fn gear_of(_ui: &Ui, _name: &str) -> (String, String) {
    (String::new(), String::new())
}

/// Makes `model` hold `rows`, touching only the rows that changed.
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let count = rows.len();
    for (i, row) in rows.into_iter().enumerate() {
        if i >= model.row_count() {
            model.push(row);
        } else if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
    while model.row_count() > count {
        model.remove(model.row_count() - 1);
    }
}

struct Ui {
    models: Models,
    overlays: Vec<OverlayWindow>,
    /// When the running sound test last played (None: no test).
    sound_test: Option<Instant>,
    settings: Settings,
    snapshot: Snapshot,
    seen_version: u64,
    view_session: usize,
    last_beep: Instant,
    sessions_shown: usize,
    sessions_count: usize,
    loader: TileLoader,
    map: MapState,
    commands: SyncSender<WorkerCommand>,
    shared: Arc<Shared>,
    filters: Filters,
}

#[derive(Default)]
struct Filters {
    resource_query: String,
    monster_query: String,
    /// The search of the map overlay.
    overlay_query: String,
    expanded: std::collections::BTreeSet<String>,
}

fn displayed_map(ui: &Ui) -> &'static MapInfo {
    ui.map
        .manual
        .or_else(|| ui.snapshot.position.as_ref().and_then(|p| p.map))
        .or_else(|| ui.settings.last_map.as_deref().and_then(maps::by_key))
        .unwrap_or_else(|| &maps::all()[0])
}

fn map_center(map: &MapInfo) -> (f64, f64) {
    let half = map.native_size() / 2.0;
    map.to_world(half, half)
}

fn to_tiles(quads: Vec<map::TileQuad>) -> ModelRc<MapTile> {
    let tiles: Vec<MapTile> =
        quads.into_iter().map(|q| MapTile { image: q.image, x: q.x.floor() as f32, y: q.y.floor() as f32, size: (q.size.ceil() + 1.0) as f32 }).collect();
    ModelRc::new(VecModel::from(tiles))
}

fn to_markers(markers: &[Marker]) -> ModelRc<MapMarker> {
    let list: Vec<MapMarker> =
        markers.iter().map(|m| MapMarker { x: m.x, y: m.y, size: m.size, color: rgb(m.color), live: m.live, level: m.level }).collect();
    ModelRc::new(VecModel::from(list))
}

struct ShownRow {
    id: i32,
    name: String,
    class: Option<String>,
    damage: i64,
    dps: Option<f64>,
    heal: i64,
    hits: u32,
    crits: u32,
    local: bool,
    estimated: bool,
    in_group: bool,
}

fn selected_session(ui: &Ui) -> Option<&za2_core::sessions::SessionSummary> {
    let list = &ui.snapshot.sessions;
    (ui.view_session > 0 && ui.view_session <= list.len()).then(|| &list[list.len() - ui.view_session])
}

fn session_label(s: &za2_core::sessions::SessionSummary) -> String {
    let start = za2_core::recording::format_time(s.start);
    format!("{} {}  {}  {}", &start[5..10], &start[11..16], s.zone_name, compact(s.total()))
}

fn apply_meter(app: &App, ui: &Ui) {
    let s = &ui.snapshot;
    let scope = ui.settings.scope;
    let history = selected_session(ui);
    let fight = ui.settings.meter_range == 0;
    let (current, current_seconds) = if fight { (&s.fight_rows, s.fight_seconds) } else { (&s.rows, s.encounter_seconds) };
    let all: Vec<ShownRow> = match history {
        Some(h) => h
            .rows
            .iter()
            .map(|r| ShownRow {
                id: r.id,
                name: r.name.clone(),
                class: r.class.clone(),
                damage: r.damage,
                dps: r.dps,
                heal: r.heal,
                hits: r.hits,
                crits: r.crits,
                local: r.local,
                estimated: false,
                in_group: true,
            })
            .collect(),
        None => current
            .iter()
            .map(|r| ShownRow {
                id: r.id,
                name: r.name.clone(),
                class: r.class.map(str::to_string),
                damage: r.damage,
                dps: r.dps,
                heal: r.heal,
                hits: r.hits,
                crits: r.crits,
                local: r.local,
                estimated: r.estimated,
                in_group: r.in_group || !s.group_known,
            })
            .collect(),
    };
    let rows: Vec<&ShownRow> = all
        .iter()
        .filter(|r| match scope {
            1 => r.in_group,
            2 => r.local,
            _ => true,
        })
        .collect();
    let total: i64 = rows.iter().map(|r| r.damage).sum();
    let top = rows.iter().map(|r| r.damage).max().unwrap_or(0).max(1);
    let model: Vec<MeterRow> = rows
        .iter()
        .map(|r| MeterRow {
            id: r.id,
            gear_score: gear_of(ui, &r.name).0.into(),
            power: gear_of(ui, &r.name).1.into(),
            name: r.name.clone().into(),
            class_name: r.class.clone().unwrap_or_default().into(),
            damage: compact(r.damage).into(),
            dps: r.dps.map_or("-".into(), |d| compact(d.round() as i64)).into(),
            share: r.damage as f32 / top as f32,
            share_text: if total > 0 { format!("{:.0}%", 100.0 * r.damage as f64 / total as f64) } else { String::new() }.into(),
            heal: if r.heal > 0 { compact(r.heal) } else { String::new() }.into(),
            crit: if r.hits > 0 { format!("{:.0}%", 100.0 * r.crits as f64 / r.hits as f64) } else { String::new() }.into(),
            local: r.local,
            estimated: r.estimated,
            color: class_color(r.class.as_deref()),
        })
        .collect();
    let target = match (&s.target, history) {
        (Some((name, hp, max)), None) if *max > 0 => format!("   target {name} {:.0}%", 100.0 * *hp as f64 / *max as f64),
        _ => String::new(),
    };
    let seconds = history.map_or(current_seconds, |h| h.fight_seconds);
    let you = s.local_name.as_ref().map(|n| format!("you: {n}")).unwrap_or_else(|| "own character not identified yet".into());
    let what = match (history.is_some(), fight) {
        (true, _) => "past session",
        (false, true) => "current fight",
        (false, false) => "total since entering the zone",
    };
    let info = format!("{what}{}  ·  {you}", target.replace("   target", "  ·  target"));
    for o in &ui.overlays {
        if let Some(cfg) = ui.settings.overlays.iter().find(|c| c.id == o.id) {
            apply_overlay(o, cfg, ui);
        }
    }
    sync_model(&ui.models.rows, model);
    app.set_encounter_time(duration_text(seconds).into());
    app.set_encounter_total(format!("{} total", compact(total)).into());
    app.set_encounter_info(info.into());

    if history.is_some() {
        sync_model(&ui.models.skills, Vec::new());
        app.set_skills_title("Skill details are kept for the current encounter only".into());
        return;
    }
    let skill_total: i64 = s.skills.iter().map(|k| k.total).sum::<i64>().max(1);
    let skills: Vec<SkillRow> = s
        .skills
        .iter()
        .map(|k| SkillRow {
            name: k.skill.clone().into(),
            total: compact(k.total).into(),
            hits: k.hits.to_string().into(),
            crit: if k.hits > 0 { format!("{:.0}%", 100.0 * k.crit_hits as f64 / k.hits as f64) } else { String::new() }.into(),
            share: k.total as f32 / skill_total as f32,
            min_max: format!("{} - {}", compact(k.min), compact(k.max)).into(),
        })
        .collect();
    sync_model(&ui.models.skills, skills);
    let title = if s.skills_of >= 0 {
        s.rows.iter().find(|r| r.id == s.skills_of).map(|r| format!("Skills of {}", r.name)).unwrap_or_else(|| "Skills".into())
    } else {
        "Select a player to see the skills".into()
    };
    app.set_skills_title(title.into());
}

fn apply_status(app: &App, ui: &Ui) {
    let s = &ui.snapshot;
    let (text, color) = match s.state {
        CaptureState::Connected => ("capturing", Color::from_rgb_u8(0x68, 0xcb, 0xa4)),
        CaptureState::Waiting | CaptureState::Starting => ("waiting", Color::from_rgb_u8(0xe5, 0xc6, 0x8a)),
        CaptureState::Replay => ("replay", Color::from_rgb_u8(0xa8, 0xa0, 0xff)),
        CaptureState::Finished => ("replay done", Color::from_rgb_u8(0x96, 0x98, 0xa8)),
        CaptureState::Error => ("error", Color::from_rgb_u8(0xf1, 0x7b, 0x87)),
    };
    app.set_capture_state(format!("{text}: {}", s.status).into());
    app.set_capture_color(color);
    app.set_zone_name(s.zone_name.clone().into());
    match &s.recording {
        Some((path, count)) => {
            app.set_recording(true);
            app.set_recording_text(format!("{count} segments to {}", path.display()).into());
        }
        None => {
            app.set_recording(false);
            app.set_recording_text(SharedString::new());
        }
    }
}

/// Whether `name` contains every word of `query`, ignoring case.
fn matches_query(name: &str, query: &str) -> bool {
    let name = name.to_lowercase();
    query.split_whitespace().all(|word| name.contains(&word.to_lowercase()))
}

/// Gathering items of a group that have places on `map` and match the search.
fn resource_items(kind: &str, map: &MapInfo, query: &str) -> Vec<&'static gather::GatherItem> {
    gather::data()
        .items
        .iter()
        .filter(|i| i.kind == kind && !gather::points_of(&map.table, &i.key).is_empty() && matches_query(i.name_in("en"), query))
        .collect()
}

/// Pet monsters of the map with their names, filtered by the search, sorted by name.
fn monster_list(map: &MapInfo, query: &str) -> Vec<(i32, String)> {
    let mut list: Vec<(i32, String)> = pets::pets_on_map(&map.table)
        .into_iter()
        .map(|id| (id, pets::pet_name(id, "en").map(str::to_string).unwrap_or_else(|| format!("#{id}"))))
        .filter(|(_, name)| matches_query(name, query))
        .collect();
    list.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    list
}

/// The map the player stands on, else the one the app shows.
fn player_map(ui: &Ui) -> &'static MapInfo {
    ui.snapshot.position.as_ref().and_then(|p| p.map).unwrap_or_else(|| displayed_map(ui))
}

/// Puts the ticked rows first when nothing is searched (so they are quick to untick); a search keeps its own order.
fn ticked_first(rows: &mut [FilterRow], searching: bool) {
    if !searching {
        rows.sort_by_key(|r| !r.checked);
    }
}

/// The map overlay's search: resources and monsters of the player's map; with no search, the ticked ones first.
fn apply_overlay_search(overlay: &MapOverlay, ui: &Ui) {
    let map = player_map(ui);
    let query = ui.filters.overlay_query.trim();
    let wanted = |name: &str| query.is_empty() || matches_query(name, query);
    let mut rows: Vec<FilterRow> = Vec::new();
    for item in &gather::data().items {
        let places = gather::points_of(&map.table, &item.key).len();
        let on = ui.settings.gather_items.contains(&item.key);
        if places > 0 && wanted(item.name_in("en")) {
            rows.push(FilterRow {
                key: format!("g:{}", item.key).into(),
                label: item.name_in("en").into(),
                detail: places.to_string().into(),
                checked: on,
                color: rgb(map::gather_color(&item.kind)),
                ..Default::default()
            });
        }
    }
    for (id, name) in monster_list(map, "") {
        let on = ui.settings.pets.contains(&id);
        if wanted(&name) {
            rows.push(FilterRow { key: format!("p:{id}").into(), label: name.into(), detail: "monster".into(), checked: on, color: rgb((0xE0, 0x50, 0x50)), ..Default::default() });
        }
    }
    ticked_first(&mut rows, !query.is_empty());
    rows.truncate(300);
    overlay.set_search_rows(ModelRc::new(VecModel::from(rows)));
}

fn apply_filters(app: &App, ui: &Ui) {
    let map = displayed_map(ui);
    let query = ui.filters.resource_query.trim();
    let selected = &ui.settings.gather_items;
    let mut rows: Vec<FilterRow> = Vec::new();
    let mut chosen = 0;
    for kind in gather::KINDS {
        let all = resource_items(kind, map, "");
        if all.is_empty() {
            continue;
        }
        chosen += all.iter().filter(|i| selected.contains(&i.key)).count();
        let shown = if query.is_empty() { all.clone() } else { resource_items(kind, map, query) };
        if shown.is_empty() {
            continue;
        }
        let places: usize = all.iter().map(|i| gather::points_of(&map.table, &i.key).len()).sum();
        let picked = shown.iter().filter(|i| selected.contains(&i.key)).count();
        let expanded = !query.is_empty() || ui.filters.expanded.contains(kind);
        rows.push(FilterRow {
            key: (*kind).into(),
            label: map::kind_label(kind).into(),
            detail: places.to_string().into(),
            header: true,
            expanded,
            checked: picked == shown.len(),
            partial: picked > 0 && picked < shown.len(),
            color: rgb(map::gather_color(kind)),
        });
        if expanded {
            let mut shown = shown;
            if query.is_empty() {
                shown.sort_by_key(|i| !selected.contains(&i.key));
            }
            for item in shown {
                let n = gather::points_of(&map.table, &item.key).len();
                rows.push(FilterRow {
                    key: item.key.clone().into(),
                    label: item.name_in("en").into(),
                    detail: n.to_string().into(),
                    header: false,
                    expanded: false,
                    checked: selected.contains(&item.key),
                    partial: false,
                    color: rgb(map::gather_color(&item.kind)),
                });
            }
        }
    }
    let summary = match (rows.is_empty(), chosen) {
        (true, _) => "nothing matches".to_string(),
        (false, 0) => "nothing selected".to_string(),
        (false, n) => format!("{n} selected"),
    };
    app.set_resource_rows(ModelRc::new(VecModel::from(rows)));
    app.set_resource_summary(summary.into());

    // The map overlay gives the selected pets of this map palette colours in id order; the list uses the same.
    let monsters = monster_list(map, ui.filters.monster_query.trim());
    let mut colored: Vec<i32> = pets::pets_on_map(&map.table).into_iter().filter(|id| ui.settings.pets.contains(id)).collect();
    colored.sort_unstable();
    let spawns = pets::spawn_table(&map.table);
    let monster_rows: Vec<FilterRow> = monsters
        .iter()
        .map(|(id, name)| {
            let points: usize = spawns
                .map(|t| t.iter().filter(|(npc, _)| pets::pets_of_npc(**npc).contains(id)).map(|(_, p)| p.len()).sum())
                .unwrap_or(0);
            let color = colored.iter().position(|c| c == id).map_or((255, 255, 255), |i| map::PET_PALETTE[i % map::PET_PALETTE.len()]);
            FilterRow {
                key: id.to_string().into(),
                label: name.clone().into(),
                detail: format!("{points} spawns").into(),
                header: false,
                expanded: false,
                checked: ui.settings.pets.contains(id),
                partial: false,
                color: rgb(color),
            }
        })
        .collect();
    let mut monster_rows = monster_rows;
    ticked_first(&mut monster_rows, !ui.filters.monster_query.trim().is_empty());
    let summary = format!("{} shown  ·  {} selected", monster_rows.len(), colored.len());
    app.set_monster_rows(ModelRc::new(VecModel::from(monster_rows)));
    app.set_monster_summary(summary.into());
}

fn apply_map(app: &App, ui: &mut Ui, force: bool) {
    let map = displayed_map(ui);
    let position = ui.snapshot.position.clone();
    let tracked_here = position.as_ref().is_some_and(|p| p.map == Some(map) && p.state != PositionState::Unknown);
    if ui.map.follow {
        if let Some(p) = position.as_ref().filter(|_| tracked_here) {
            ui.map.center = (p.x as f64, p.y as f64);
        }
    }
    let width = app.get_map_width() as f64;
    let height = app.get_map_height() as f64;
    if width < 10.0 || height < 10.0 {
        return;
    }
    let key = (
        map.key.clone(),
        (ui.map.center.0.round() as i64, ui.map.center.1.round() as i64),
        (ui.settings.map_radius_metres * 10.0) as i64,
        (width as i64, height as i64),
        ui.snapshot.version,
    );
    if !force && ui.map.last_built.as_ref() == Some(&key) {
        return;
    }
    ui.map.last_built = Some(key);

    let view = View { center_x: ui.map.center.0, center_y: ui.map.center.1, radius_metres: ui.settings.map_radius_metres, width, height };
    let faction = faction_of(&ui.snapshot);
    ui.map.layer.update(Some(map), &ui.settings.gather_items, &ui.settings.pets, faction);
    let quads = map::visible_tiles(map, &view, &mut ui.loader);
    let known = tracked_here && position.as_ref().is_some_and(|p| matches!(p.state, PositionState::Exact | PositionState::Estimated));
    let live = LiveInputs {
        known,
        player: position.as_ref().map_or((0.0, 0.0), |p| (p.x, p.y)),
        player_z: position.as_ref().and_then(|p| p.z),
        env: &ui.snapshot.live_env,
        mobs: &ui.snapshot.live_mobs,
        show_live_mobs: ui.settings.show_live_mobs,
        show_cubes: ui.settings.show_cubes,
        pet_ids: &ui.settings.pets,
    };
    map::markers(map, &view, &ui.map.layer, &live, &mut ui.map.markers);
    app.set_tiles(to_tiles(quads));
    app.set_markers(to_markers(&ui.map.markers));

    match position.as_ref().filter(|_| tracked_here) {
        Some(p) => {
            let (x, y) = view.to_screen(map, p.x as f64, p.y as f64);
            app.set_me_visible(true);
            app.set_me_x(x as f32);
            app.set_me_y(y as f32);
            app.set_me_heading(p.heading as f32);
            app.set_me_color(state_color(p.state));
            app.set_me_label(if p.state == PositionState::Stale { "?".into() } else { SharedString::new() });
        }
        None => app.set_me_visible(false),
    }
    let state = position.as_ref().map_or(PositionState::Unknown, |p| p.state);
    let age = position.as_ref().and_then(|p| p.raw).map(|r| (ui.snapshot.now - r.at) as f64 / 1e6);
    let mut text = match (state, position.as_ref().and_then(|p| p.raw).map(|r| r.source)) {
        (PositionState::Estimated, Some(PositionSource::Combat)) => "Estimated from your target".to_string(),
        _ => state_text(state).to_string(),
    };
    if let (Some(a), PositionState::Exact | PositionState::Stale) = (age, state) {
        text.push_str(&format!(" ({a:.0}s)"));
    }
    if let Some(p) = position.as_ref() {
        if let Some(m) = p.map {
            if m != map {
                text.push_str(&format!(" on {}", m.title));
            }
        } else if state != PositionState::Unknown {
            text.push_str(", map not detected");
        }
    }
    app.set_position_text(text.into());
    app.set_position_color(state_color(state));
    let index = maps::all().iter().position(|m| m == map).unwrap_or(0) as i32;
    if app.get_map_index() != index {
        app.set_map_index(index);
    }
    app.set_map_hint(
        format!(
            "{}  radius {:.0} m  drag to pan, wheel to zoom{}{}",
            map.title,
            ui.settings.map_radius_metres,
            if ui.loader.loading() { "  loading tiles" } else { "" },
            if state == PositionState::Unknown { "  |  no position yet: walk near monsters, or teleport" } else { "" }
        )
        .into(),
    );

    if let Some(p) = position.as_ref().filter(|p| p.map.is_some() && matches!(p.state, PositionState::Exact | PositionState::Estimated)) {
        if ui.map.last_saved.elapsed() >= Duration::from_secs(10) {
            ui.map.last_saved = Instant::now();
            let key = p.map.map(|m| m.key.clone());
            let moved = ui.settings.last_map != key
                || ui.settings.last_x.zip(ui.settings.last_y).map_or(true, |(x, y)| (x - p.x).abs() + (y - p.y).abs() >= 500.0);
            if moved {
                ui.settings.last_map = key;
                ui.settings.last_x = Some(p.x);
                ui.settings.last_y = Some(p.y);
                ui.settings.save();
            }
        }
    }
}

fn apply_map_overlay(overlay: &MapOverlay, ui: &mut Ui, force: bool) {
    let Some(p) = ui.snapshot.position.clone() else { return };
    let Some(map) = p.map else {
        overlay.set_tiles(ModelRc::new(VecModel::from(Vec::<MapTile>::new())));
        overlay.set_markers(ModelRc::new(VecModel::from(Vec::<MapMarker>::new())));
        overlay.set_me_visible(false);
        return;
    };
    let width = overlay.get_view_width() as f64;
    let height = overlay.get_view_height() as f64;
    if width < 10.0 || height < 10.0 {
        return;
    }
    let key = (map.key.clone(), (p.x as i64, p.y as i64), (width as i64, height as i64), ui.snapshot.version);
    if !force && ui.map.last_overlay.as_ref() == Some(&key) {
        return;
    }
    ui.map.last_overlay = Some(key);
    let view = View { center_x: p.x as f64, center_y: p.y as f64, radius_metres: ui.settings.overlay_radius_metres, width, height };
    ui.map.overlay_layer.update(Some(map), &ui.settings.gather_items, &ui.settings.pets, faction_of(&ui.snapshot));
    let quads = map::visible_tiles(map, &view, &mut ui.loader);
    let live = LiveInputs {
        known: matches!(p.state, PositionState::Exact | PositionState::Estimated),
        player: (p.x, p.y),
        player_z: p.z,
        env: &ui.snapshot.live_env,
        mobs: &ui.snapshot.live_mobs,
        show_live_mobs: ui.settings.show_live_mobs,
        show_cubes: ui.settings.show_cubes,
        pet_ids: &ui.settings.pets,
    };
    let mut markers = Vec::new();
    map::markers(map, &view, &ui.map.overlay_layer, &live, &mut markers);
    overlay.set_tiles(to_tiles(quads));
    overlay.set_markers(to_markers(&markers));
    overlay.set_me_visible(p.state != PositionState::Unknown);
    overlay.set_me_x((width / 2.0) as f32);
    overlay.set_me_y((height / 2.0) as f32);
    overlay.set_me_heading(p.heading as f32);
    overlay.set_me_color(state_color(p.state));
    overlay.set_me_label(if p.state == PositionState::Stale { "?".into() } else { SharedString::new() });
}

/// Zooms the map overlay: a positive `delta` (wheel up, the + button) zooms in.
fn zoom_map_overlay(app: &App, overlay: &MapOverlay, ui: &mut Ui, delta: f32) {
    let radius = (ui.settings.overlay_radius_metres * (-(delta as f64) * 0.0025).exp()).clamp(25.0, 1000.0);
    ui.settings.overlay_radius_metres = radius;
    ui.settings.save();
    app.set_overlay_radius(radius as f32);
    apply_map_overlay(overlay, ui, true);
}

fn cube_beep(ui: &mut Ui) {
    if !ui.settings.cube_sound_enabled {
        return;
    }
    let Some(p) = ui.snapshot.position.as_ref().and_then(|p| p.raw) else { return };
    if ui.snapshot.now - p.at >= 5_000_000 {
        return;
    }
    let player_z = ui.snapshot.position.as_ref().and_then(|p| p.z);
    let nearest = ui
        .snapshot
        .live_env
        .iter()
        .filter(|e| e.kind == "Cube")
        .map(|e| {
            let dz = match player_z {
                Some(z) if e.z.is_finite() => (e.z - z) as f64,
                _ => 0.0,
            };
            (((e.x - p.x) as f64).powi(2) + ((e.y - p.y) as f64).powi(2) + dz * dz).sqrt() / 100.0
        })
        .fold(f64::INFINITY, f64::min);
    let Some(interval) = cube_beep_interval(nearest) else { return };
    if ui.last_beep.elapsed().as_secs_f64() < interval {
        return;
    }
    ui.last_beep = Instant::now();
    play_cube_sound(&ui.settings);
}

/// Within this many metres the player stands at the cube and is picking it up: the sound stops.
const CUBE_SILENT_METRES: f64 = 3.0;

/// Seconds between beeps for the nearest cube `metres` away: faster when closer, silent at the cube.
fn cube_beep_interval(metres: f64) -> Option<f64> {
    (metres.is_finite() && metres >= CUBE_SILENT_METRES).then(|| (0.15 + (metres - 4.0) / 56.0 * 1.85).clamp(0.15, 2.0))
}

fn play_cube_sound(settings: &Settings) {
    let file = za2_core::assets::path(&format!("sounds/{}.wav", settings.cube_sound));
    if !file.is_file() {
        return;
    }
    let volume = (settings.cube_sound_volume.min(100) as f64 / 100.0).to_string();
    std::thread::spawn(move || {
        let played = std::process::Command::new("pw-play").arg("--volume").arg(&volume).arg(&file).status().is_ok_and(|s| s.success());
        if !played {
            let _ = std::process::Command::new("paplay").arg(&file).status();
        }
    });
}

/// Hands a left-button press to the compositor to move the window. The compositor keeps the button release, so
/// Slint would still see the button held and send every later left click to the element that started the move
/// (a right click reset it). Telling Slint the button went up ends that. It has to happen after Slint finished
/// the press (which is when it starts holding the element), so on the next turn of the event loop.
fn move_window<C: ComponentHandle + 'static>(component: &C) {
    let _ = component.window().with_winit_window(|w| w.drag_window());
    release_left_button_later(component);
}

fn resize_window<C: ComponentHandle + 'static>(component: &C, direction: winit::window::ResizeDirection) {
    let _ = component.window().with_winit_window(|w| w.drag_resize_window(direction));
    release_left_button_later(component);
}

fn release_left_button_later<C: ComponentHandle + 'static>(component: &C) {
    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        let Some(component) = weak.upgrade() else { return };
        let window = component.window();
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(0.0, 0.0),
            button: slint::platform::PointerEventButton::Left,
        });
        window.dispatch_event(slint::platform::WindowEvent::PointerExited);
    });
}

fn set_click_through(window: &slint::Window, locked: bool) -> Result<(), String> {
    let result = window
        .with_winit_window(|w| {
            w.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
            w.set_cursor_hittest(!locked).map_err(|e| e.to_string())
        })
        .unwrap_or_else(|| Err("the window is not backed by winit".into()));
    if let Err(e) = &result {
        eprintln!("za2: click-through: {e}");
    }
    result
}

fn main() {
    let args = parse_args();
    if let Some(command) = &args.send {
        match control::send(command) {
            Ok(()) => return,
            Err(e) => {
                eprintln!("za2: {e}");
                std::process::exit(1);
            }
        }
    }
    if args.x11 {
        std::env::remove_var("WAYLAND_DISPLAY");
    }

    desktop::install();
    let settings = Settings::load();
    let source = match &args.replay {
        Some(path) => Source::Replay { path: path.clone(), speed: args.speed },
        None => Source::Live {
            helper: helper_path(args.helper.clone(), "ZA2_CAPTURE", "za2-capture"),
            device: settings.capture_device.clone(),
            memory_helper: (!args.no_memory).then(|| helper_path(args.memory_helper.clone(), "ZA2_MEMORY", "za2-memory")),
        },
    };
    let (shared, commands) = worker::start(source, settings.own_character_name.clone(), settings.reset_on_zone_change);
    let _ = commands.send(WorkerCommand::RestoreMap(settings.last_map.clone(), settings.last_x.zip(settings.last_y)));

    let app = App::new().expect("cannot create the main window");
    let map_overlay = MapOverlay::new().expect("cannot create the map overlay");

    let start_map = settings.last_map.as_deref().and_then(maps::by_key).unwrap_or_else(|| &maps::all()[0]);
    let center = match (settings.last_x, settings.last_y) {
        (Some(x), Some(y)) => (x as f64, y as f64),
        _ => map_center(start_map),
    };
    let ui = Rc::new(RefCell::new(Ui {
        settings: settings.clone(),
        snapshot: Snapshot::default(),
        seen_version: 0,
        view_session: 0,
        last_beep: Instant::now(),
        sessions_shown: usize::MAX,
        sessions_count: 0,
        loader: TileLoader::new(),
        map: MapState {
            manual: None,
            center,
            follow: true,
            layer: StaticLayer::default(),
            overlay_layer: StaticLayer::default(),
            markers: Vec::new(),
            last_saved: Instant::now(),
            last_built: None,
            last_overlay: None,
        },
        commands: commands.clone(),
        shared: shared.clone(),
        filters: Filters::default(),
        models: Models::new(&app),
        overlays: Vec::new(),
        sound_test: None,
    }));

    app.set_map_names(ModelRc::new(VecModel::from(maps::all().iter().map(|m| SharedString::from(m.title.as_str())).collect::<Vec<_>>())));
    app.set_own_name(settings.own_character_name.clone().into());
    app.set_reset_on_zone(settings.reset_on_zone_change);
    app.set_scope(settings.scope);
    app.set_show_live_mobs(settings.show_live_mobs);
    app.set_show_cubes(settings.show_cubes);
    app.set_cube_sound(settings.cube_sound_enabled);
    app.set_cube_volume(settings.cube_sound_volume as f32);
    app.set_meter_overlay(settings.meter_overlay);
    app.set_map_overlay(settings.map_overlay);
    app.set_overlays_locked(settings.overlays_locked);
    app.set_overlay_opacity(settings.overlay_opacity);
    app.set_overlay_radius(settings.overlay_radius_metres as f32);
    app.set_tab(if args.tab == 2 { 0 } else { args.tab });
    app.set_settings_open(args.tab == 2);
    app.set_meter_range(settings.meter_range as i32);
    map_overlay.set_locked(settings.overlays_locked);
    map_overlay.set_overlay_opacity(settings.overlay_opacity);
    apply_filters(&app, &ui.borrow());

    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_session_selected(move |index| {
            let mut u = ui.borrow_mut();
            u.view_session = index.max(0) as usize;
            apply_meter(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        app.on_reset(move || {
            let _ = ui.borrow().commands.send(WorkerCommand::Reset);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_select_row(move |id| {
            let app = weak.unwrap();
            let next = if app.get_selected_row() == id { -1 } else { id };
            app.set_selected_row(next);
            let _ = ui.borrow().commands.send(WorkerCommand::SelectSource(next));
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_scope_changed(move |scope| {
            let mut u = ui.borrow_mut();
            u.settings.scope = scope;
            u.settings.save();
            apply_meter(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_meter_range_changed(move |range| {
            let mut u = ui.borrow_mut();
            u.settings.meter_range = range.clamp(0, 1) as u8;
            u.settings.save();
            apply_meter(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_overlay_add(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let id = u.settings.overlays.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            u.settings.overlays.push(settings::OverlayConfig { id, ..Default::default() });
            u.settings.save();
            sync_overlays(&app, &mut u);
            apply_meter(&app, &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_overlay_remove(move |id| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.settings.overlays.retain(|c| c.id as i32 != id);
            u.settings.save();
            sync_overlays(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_overlay_changed(move |row| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            if let Some(c) = u.settings.overlays.iter_mut().find(|c| c.id as i32 == row.id) {
                c.enabled = row.enabled;
                c.metric = row.metric.clamp(0, 1) as u8;
                c.range = row.range.clamp(0, 1) as u8;
                c.scope = row.scope.clamp(0, 2) as u8;
            }
            u.settings.save();
            sync_overlays(&app, &mut u);
            apply_meter(&app, &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_pan_by(move |dx, dy| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let width = app.get_map_width() as f64;
            let height = app.get_map_height() as f64;
            let view = View { center_x: 0.0, center_y: 0.0, radius_metres: u.settings.map_radius_metres, width, height };
            let ppu = view.pixels_per_unit();
            let map = displayed_map(&u);
            u.map.center.0 -= dx as f64 / ppu;
            u.map.center.1 -= dy as f64 / ppu;
            u.map.manual = Some(map);
            u.map.follow = false;
            app.set_follow(false);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_zoom_by(move |delta, mx, my| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let width = app.get_map_width() as f64;
            let height = app.get_map_height() as f64;
            let before = View { center_x: u.map.center.0, center_y: u.map.center.1, radius_metres: u.settings.map_radius_metres, width, height };
            let ppu0 = before.pixels_per_unit();
            let anchor = (u.map.center.0 + (mx as f64 - width / 2.0) / ppu0, u.map.center.1 + (my as f64 - height / 2.0) / ppu0);
            let radius = (u.settings.map_radius_metres * (-(delta as f64) * 0.0025).exp()).clamp(25.0, 8000.0);
            u.settings.map_radius_metres = radius;
            let ppu1 = View { radius_metres: radius, ..before }.pixels_per_unit();
            if !u.map.follow {
                u.map.center = (anchor.0 - (mx as f64 - width / 2.0) / ppu1, anchor.1 - (my as f64 - height / 2.0) / ppu1);
            }
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_map_selected(move |index| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            if let Some(map) = maps::all().get(index as usize) {
                u.map.manual = Some(map);
                u.map.follow = false;
                u.map.center = map_center(map);
                u.settings.map_radius_metres = map.native_size() / 2.0 / map.scale / 100.0;
                app.set_follow(false);
                apply_filters(&app, &u);
                apply_map(&app, &mut u, true);
            }
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_follow_changed(move |on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.map.follow = on;
            if on {
                u.map.manual = None;
                if u.settings.map_radius_metres > 1500.0 {
                    u.settings.map_radius_metres = 250.0;
                }
            }
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_center_on_player(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            if let Some(p) = u.snapshot.position.clone().filter(|p| p.map.is_some() && p.state != PositionState::Unknown) {
                u.map.manual = p.map;
                u.map.center = (p.x as f64, p.y as f64);
                apply_map(&app, &mut u, true);
            }
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_toggle_kind(move |kind, on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let keys: Vec<String> = resource_items(kind.as_str(), displayed_map(&u), u.filters.resource_query.trim()).iter().map(|i| i.key.clone()).collect();
            for k in keys {
                if on {
                    u.settings.gather_items.insert(k);
                } else {
                    u.settings.gather_items.remove(&k);
                }
            }
            u.settings.save();
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_toggle_gather(move |key, on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            if on {
                u.settings.gather_items.insert(key.to_string());
            } else {
                u.settings.gather_items.remove(key.as_str());
            }
            u.settings.save();
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_toggle_pet(move |key, on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            if let Ok(id) = key.parse::<i32>() {
                if on {
                    u.settings.pets.insert(id);
                } else {
                    u.settings.pets.remove(&id);
                }
            }
            u.settings.save();
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_expand_kind(move |kind| {
            let mut u = ui.borrow_mut();
            let kind = kind.to_string();
            if !u.filters.expanded.remove(&kind) {
                u.filters.expanded.insert(kind);
            }
            apply_filters(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_resource_search(move |text| {
            let mut u = ui.borrow_mut();
            u.filters.resource_query = text.to_string();
            apply_filters(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_monster_search(move |text| {
            let mut u = ui.borrow_mut();
            u.filters.monster_query = text.to_string();
            apply_filters(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_select_shown_monsters(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let shown: Vec<i32> = monster_list(displayed_map(&u), u.filters.monster_query.trim()).into_iter().map(|m| m.0).collect();
            u.settings.pets.extend(shown);
            u.settings.save();
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_clear_monsters(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.settings.pets.clear();
            u.settings.save();
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_clear_resources(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.settings.gather_items.clear();
            u.settings.save();
            apply_filters(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let mapo = map_overlay.as_weak();
        app.on_overlay_radius_changed(move |radius| {
            let mut u = ui.borrow_mut();
            u.settings.overlay_radius_metres = radius as f64;
            u.settings.save();
            apply_map_overlay(&mapo.unwrap(), &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        let mapo = map_overlay.as_weak();
        map_overlay.on_zoom_by(move |delta| zoom_map_overlay(&weak.unwrap(), &mapo.unwrap(), &mut ui.borrow_mut(), delta));
    }
    {
        let ui = ui.clone();
        let mapo = map_overlay.as_weak();
        map_overlay.on_search(move |text| {
            let mut u = ui.borrow_mut();
            u.filters.overlay_query = text.to_string();
            apply_overlay_search(&mapo.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        let mapo = map_overlay.as_weak();
        map_overlay.on_toggle_item(move |key, on| {
            let (app, o) = (weak.unwrap(), mapo.unwrap());
            let mut u = ui.borrow_mut();
            if let Some(item) = key.strip_prefix("g:") {
                if on {
                    u.settings.gather_items.insert(item.to_string());
                } else {
                    u.settings.gather_items.remove(item);
                }
            } else if let Some(id) = key.strip_prefix("p:").and_then(|id| id.parse::<i32>().ok()) {
                if on {
                    u.settings.pets.insert(id);
                } else {
                    u.settings.pets.remove(&id);
                }
            }
            u.settings.save();
            apply_filters(&app, &u);
            apply_overlay_search(&o, &u);
            apply_map(&app, &mut u, true);
            apply_map_overlay(&o, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        let mapo = map_overlay.as_weak();
        app.on_map_overlay_zoom(move |delta| zoom_map_overlay(&weak.unwrap(), &mapo.unwrap(), &mut ui.borrow_mut(), delta));
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_live_mobs_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.show_live_mobs = on;
            u.settings.save();
            apply_map(&weak.unwrap(), &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_cubes_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.show_cubes = on;
            u.settings.save();
            apply_map(&weak.unwrap(), &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        app.on_test_cube_sound(move |on| {
            let mut u = ui.borrow_mut();
            u.sound_test = on.then(Instant::now);
            if on {
                play_cube_sound(&u.settings);
            }
        });
    }
    {
        let ui = ui.clone();
        app.on_cube_volume_changed(move |volume| {
            let mut u = ui.borrow_mut();
            u.settings.cube_sound_volume = volume.round().clamp(0.0, 100.0) as u32;
            u.settings.save();
        });
    }
    {
        let ui = ui.clone();
        app.on_cube_sound_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.cube_sound_enabled = on;
            u.settings.save();
        });
    }
    {
        let ui = ui.clone();
        app.on_own_name_edited(move |name| {
            let mut u = ui.borrow_mut();
            u.settings.own_character_name = name.trim().to_string();
            u.settings.save();
            let _ = u.commands.send(WorkerCommand::OwnName(name.trim().to_string()));
        });
    }
    {
        let ui = ui.clone();
        app.on_reset_on_zone_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.reset_on_zone_change = on;
            u.settings.save();
            let _ = u.commands.send(WorkerCommand::ResetOnZone(on));
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_meter_overlay_changed(move |on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.settings.meter_overlay = on;
            u.settings.save();
            sync_overlays(&app, &mut u);
            apply_meter(&app, &u);
        });
    }
    {
        let ui = ui.clone();
        let overlay = map_overlay.as_weak();
        app.on_map_overlay_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.map_overlay = on;
            u.settings.save();
            let o = overlay.unwrap();
            if on {
                let _ = o.show();
                let locked = u.settings.overlays_locked;
                let weak = o.as_weak();
                slint::Timer::single_shot(Duration::from_millis(300), move || {
                    if let Some(o) = weak.upgrade() {
                        let _ = set_click_through(o.window(), locked);
                    }
                });
                apply_map_overlay(&o, &mut u, true);
            } else {
                let _ = o.hide();
            }
        });
    }
    {
        let ui = ui.clone();
        let mapo = map_overlay.as_weak();
        app.on_lock_changed(move |locked| {
            let mut u = ui.borrow_mut();
            u.settings.overlays_locked = locked;
            u.settings.save();
            let o = mapo.unwrap();
            o.set_locked(locked);
            if locked {
                o.set_search_open(false);
            }
            let mut errors = Vec::new();
            if let Err(e) = set_click_through(o.window(), locked) {
                errors.push(e);
            }
            for m in &u.overlays {
                m.window.set_locked(locked);
                if m.shown {
                    if let Err(e) = set_click_through(m.window.window(), locked) {
                        errors.push(e);
                    }
                }
            }
            if !errors.is_empty() {
                eprintln!("za2: click-through is not available here: {}", errors.join("; "));
            }
        });
    }
    {
        let ui = ui.clone();
        let mapo = map_overlay.as_weak();
        app.on_opacity_changed(move |v| {
            let mut u = ui.borrow_mut();
            u.settings.overlay_opacity = v;
            u.settings.save();
            for m in &u.overlays {
                m.window.set_overlay_opacity(v);
            }
            mapo.unwrap().set_overlay_opacity(v);
        });
    }
    {
        let ui = ui.clone();
        app.on_start_recording(move || {
            let stamp = za2_core::recording::format_time(za2_core::segment::now_micros()).replace([':', '+'], "-");
            let path = settings::data_dir().join("recordings").join(format!("za2-{}.jsonl", &stamp[..19.min(stamp.len())]));
            let _ = ui.borrow().commands.send(WorkerCommand::StartRecording(path));
        });
    }
    {
        let ui = ui.clone();
        app.on_stop_recording(move || {
            let _ = ui.borrow().commands.send(WorkerCommand::StopRecording);
        });
    }
    {
        let weak = map_overlay.as_weak();
        map_overlay.on_drag(move || {
            move_window(&weak.unwrap());
        });
        let app_weak = app.as_weak();
        map_overlay.on_toggle_lock(move || {
            let app = app_weak.unwrap();
            app.set_overlays_locked(true);
            app.invoke_lock_changed(true);
        });
    }

    {
        let weak = app.as_weak();
        app.on_window_drag(move || {
            move_window(&weak.unwrap());
        });
        let weak = app.as_weak();
        app.on_window_minimize(move || {
            weak.unwrap().window().with_winit_window(|w| w.set_minimized(true));
        });
        let weak = app.as_weak();
        app.on_window_maximize(move || {
            let app = weak.unwrap();
            let maximized = !app.window().is_maximized();
            app.window().set_maximized(maximized);
            app.set_is_maximized(maximized);
        });
        app.on_window_close(|| {
            let _ = slint::quit_event_loop();
        });
        let weak = app.as_weak();
        app.on_window_resize(move |edge| {
            use winit::window::ResizeDirection as D;
            let direction = match edge {
                0 => D::North,
                1 => D::South,
                2 => D::East,
                3 => D::West,
                4 => D::NorthEast,
                5 => D::NorthWest,
                6 => D::SouthEast,
                _ => D::SouthWest,
            };
            resize_window(&weak.unwrap(), direction);
        });
    }

    let timer = slint::Timer::default();
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        let mapo = map_overlay.as_weak();
        let mut filters_for: Option<String> = None;
        let mut learned_saved = false;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            let (app, mapo) = (weak.unwrap(), mapo.unwrap());
            let mut u = ui.borrow_mut();
            let tiles_arrived = u.loader.poll();
            let maximized = app.window().is_maximized();
            if app.get_is_maximized() != maximized {
                app.set_is_maximized(maximized);
            }
            cube_beep(&mut u);
            if u.sound_test.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) {
                u.sound_test = Some(Instant::now());
                play_cube_sound(&u.settings);
            }
            let version = u.shared.version.load(Ordering::Acquire);
            let fresh = version != u.seen_version;
            if fresh {
                u.seen_version = version;
                let snapshot = u.shared.snapshot.lock().unwrap().clone();
                u.snapshot = snapshot;
                let ptr = Arc::as_ptr(&u.snapshot.sessions) as usize;
                if ptr != u.sessions_shown {
                    u.sessions_shown = ptr;
                    let count = u.snapshot.sessions.len();
                    let added = count.saturating_sub(u.sessions_count);
                    u.sessions_count = count;
                    let mut names = vec![SharedString::from("Current encounter")];
                    names.extend(u.snapshot.sessions.iter().rev().map(|s| SharedString::from(session_label(s))));
                    if u.view_session > 0 && added > 0 {
                        u.view_session = (u.view_session + added).min(count);
                        app.set_session_index(u.view_session as i32);
                    }
                    app.set_session_names(ModelRc::new(VecModel::from(names)));
                }
                apply_meter(&app, &u);
                apply_status(&app, &u);
                if let Some(name) = u.snapshot.learned_name.clone() {
                    if !learned_saved && name != u.settings.own_character_name {
                        learned_saved = true;
                        u.settings.own_character_name = name.clone();
                        u.settings.save();
                        app.set_own_name(name.into());
                    }
                }
            }
            let shown = displayed_map(&u).key.clone();
            if filters_for.as_deref() != Some(shown.as_str()) {
                filters_for = Some(shown);
                apply_filters(&app, &u);
            }
            if app.get_tab() == 1 && (fresh || tiles_arrived) {
                apply_map(&app, &mut u, tiles_arrived);
            } else if app.get_tab() == 1 {
                apply_map(&app, &mut u, false);
            }
            if u.settings.map_overlay && (fresh || tiles_arrived) {
                apply_map_overlay(&mapo, &mut u, tiles_arrived);
            }
        });
    }

    {
        let weak = app.as_weak();
        control::listen(move |command| {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = weak.upgrade() else { return };
                match command.as_str() {
                    "show" => {
                        let _ = app.show();
                    }
                    "reset" => app.invoke_reset(),
                    "toggle-overlay" => {
                        let on = !app.get_meter_overlay();
                        app.set_meter_overlay(on);
                        app.invoke_meter_overlay_changed(on);
                    }
                    "toggle-map-overlay" => {
                        let on = !app.get_map_overlay();
                        app.set_map_overlay(on);
                        app.invoke_map_overlay_changed(on);
                    }
                    "toggle-lock" => {
                        let on = !app.get_overlays_locked();
                        app.set_overlays_locked(on);
                        app.invoke_lock_changed(on);
                    }
                    "map-zoom-in" => app.invoke_map_overlay_zoom(120.0),
                    "map-zoom-out" => app.invoke_map_overlay_zoom(-120.0),
                    _ => {}
                }
            });
        });
    }

    app.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    app.show().expect("cannot show the main window");
    sync_overlays(&app, &mut ui.borrow_mut());
    if settings.map_overlay {
        app.invoke_map_overlay_changed(true);
    }
    slint::run_event_loop_until_quit().ok();
    for m in &ui.borrow().overlays {
        let _ = m.window.hide();
    }
    let _ = map_overlay.hide();
    let u = ui.borrow();
    u.settings.save();
    control::cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_sound_speeds_up_and_stops_at_the_cube() {
        assert_eq!(cube_beep_interval(f64::INFINITY), None, "no cube in view");
        assert_eq!(cube_beep_interval(2.0), None, "standing at the cube");
        let (near, far) = (cube_beep_interval(5.0).unwrap(), cube_beep_interval(40.0).unwrap());
        assert!(near < far && near >= 0.15 && far <= 2.0);
        assert_eq!(cube_beep_interval(200.0), Some(2.0));
    }
}
