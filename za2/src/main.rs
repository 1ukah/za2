mod chart;
mod control;
mod desktop;
mod icons;
mod map;
mod memory;
mod pet_tracker;
mod settings;
mod timers;
mod worker;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use za2_core::catalog::{gather, map_icons, maps, pets};
use za2_core::catalog::field_bosses::FieldBoss;
use za2_core::catalog::maps::MapInfo;
use za2_core::field_bosses::{BossState, FieldBossBoard};
use za2_core::entities::PositionSource;
use za2_core::pet_progress::{self, PetState};
use za2_core::position::PositionState;
use slint::winit_030::{winit, WinitWindowAccessor};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use map::{LiveInputs, Marker, StaticLayer, TileLoader, View};
use settings::Settings;
use pet_tracker::PetSource;
use worker::{CaptureState, Shared, Snapshot, Source, WorkerCommand};

slint::include_modules!();

/// `--tab settings` opens the settings dialog over the meter; the pets page is the fourth page, the profile the fifth.
const SETTINGS_TAB: i32 = -1;
const PETS_TAB: i32 = 3;
const PROFILE_TAB: i32 = 4;
const TIMERS_TAB: i32 = 5;

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
    eprintln!("usage: za2 [--replay recording.jsonl [--speed N]] [--x11] [--tab meter|map|timeline|pets|profile|settings] [--helper PATH] [--memory-helper PATH | --no-memory] [--assets DIR]");
    eprintln!("       za2 --version");
    eprintln!("       za2 --send show|reset|toggle-overlay|toggle-map-overlay|toggle-pet-overlay|toggle-lock|map-zoom-in|map-zoom-out");
    std::process::exit(2);
}

fn parse_args() -> Args {
    // the map is the page za2 opens on
    let mut a = Args { replay: None, speed: 1.0, x11: false, helper: None, memory_helper: None, no_memory: false, send: None, tab: 1 };
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
                    Some("timeline") => 2,
                    Some("settings") => SETTINGS_TAB,
                    Some("pets") => PETS_TAB,
                    Some("profile") => PROFILE_TAB,
                    Some("timers") => TIMERS_TAB,
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

/// The time to kill of the own target, as "TTK 1:23"; empty when there is no estimate.
fn ttk_text(s: &worker::Snapshot) -> String {
    s.time_to_kill.map(|t| format!("TTK {}", duration_text(t.ceil()))).unwrap_or_default()
}

/// The ping as "42ms" and its colour (as A2Tools grades it); empty when unknown.
fn ping_text(s: &worker::Snapshot) -> (String, Color) {
    let Some(ms) = s.ping else { return (String::new(), Color::default()) };
    let color = match ms {
        ..100 => Color::from_rgb_u8(0x68, 0xcb, 0xa4),
        ..200 => Color::from_rgb_u8(0xf2, 0xb9, 0x4f),
        ..225 => Color::from_rgb_u8(0xf2, 0x7a, 0x3a),
        _ => Color::from_rgb_u8(0xe0, 0x6b, 0x6b),
    };
    (format!("{ms}ms"), color)
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
    pet_rows: Rc<VecModel<PetRow>>,
    skills: Rc<VecModel<SkillRow>>,
    chart_lines: Rc<VecModel<ChartLine>>,
    chart_marks: Rc<VecModel<ChartMark>>,
    boss_rows: Rc<VecModel<BossRow>>,
    /// The names and return times of the shown field bosses, on the map and on the map overlay.
    boss_labels: Rc<VecModel<MapLabel>>,
    overlay_boss_labels: Rc<VecModel<MapLabel>>,
    /// The shown field bosses out of view, pinned in a corner of the map.
    boss_pins: Rc<VecModel<BossPin>>,
}

impl Models {
    fn new(app: &App) -> Models {
        let models = Models {
            rows: Rc::new(VecModel::default()),
            pet_rows: Rc::new(VecModel::default()),
            skills: Rc::new(VecModel::default()),
            chart_lines: Rc::new(VecModel::default()),
            chart_marks: Rc::new(VecModel::default()),
            boss_rows: Rc::new(VecModel::default()),
            boss_labels: Rc::new(VecModel::default()),
            overlay_boss_labels: Rc::new(VecModel::default()),
            boss_pins: Rc::new(VecModel::default()),
        };
        app.set_rows(ModelRc::from(models.rows.clone()));
        app.set_pet_rows(ModelRc::from(models.pet_rows.clone()));
        app.set_skills(ModelRc::from(models.skills.clone()));
        app.set_chart_lines(ModelRc::from(models.chart_lines.clone()));
        app.set_chart_marks(ModelRc::from(models.chart_marks.clone()));
        app.set_boss_rows(ModelRc::from(models.boss_rows.clone()));
        app.set_boss_labels(ModelRc::from(models.boss_labels.clone()));
        app.set_boss_pins(ModelRc::from(models.boss_pins.clone()));
        models
    }
}

/// A meter overlay window and the list it shows.
struct OverlayWindow {
    id: u32,
    window: MeterOverlay,
    rows: Rc<VecModel<OverlayRow>>,
    skills: Rc<VecModel<SkillRow>>,
    shown: bool,
}

/// The players whose skills are shown: the meter's selection and each overlay's. The worker computes the skill
/// breakdown of all of them.
#[derive(Default)]
struct Selections {
    meter: Cell<i32>,
    overlays: RefCell<HashMap<u32, i32>>,
}

impl Selections {
    fn new() -> Selections {
        Selections { meter: Cell::new(-1), overlays: RefCell::new(HashMap::new()) }
    }

    fn of_overlay(&self, id: u32) -> i32 {
        self.overlays.borrow().get(&id).copied().unwrap_or(-1)
    }

    fn send(&self, commands: &SyncSender<WorkerCommand>) {
        let mut ids: Vec<i32> = self.overlays.borrow().values().copied().chain([self.meter.get()]).filter(|&id| id >= 0).collect();
        ids.sort_unstable();
        ids.dedup();
        let _ = commands.send(WorkerCommand::SelectSources(ids));
    }
}

fn resize_direction(edge: i32) -> winit::window::ResizeDirection {
    use winit::window::ResizeDirection as D;
    match edge {
        0 => D::North,
        1 => D::South,
        2 => D::East,
        3 => D::West,
        4 => D::NorthEast,
        5 => D::NorthWest,
        6 => D::SouthEast,
        _ => D::SouthWest,
    }
}

/// The logical size of a window, rounded.
fn logical_size(window: &slint::Window) -> (f32, f32) {
    let size = window.size().to_logical(window.scale_factor());
    (size.width.round(), size.height.round())
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
        let skills = Rc::new(VecModel::default());
        window.set_rows(ModelRc::from(rows.clone()));
        window.set_skills(ModelRc::from(skills.clone()));
        window.set_overlay_title(format!("za2 meter overlay {}", cfg.id).into());
        if let Some((w, h)) = cfg.size {
            window.window().set_size(slint::LogicalSize::new(w, h));
        }
        let weak = window.as_weak();
        window.on_drag(move || move_window(&weak.unwrap()));
        let weak = window.as_weak();
        window.on_resize(move |edge| resize_window(&weak.unwrap(), resize_direction(edge)));
        let (selections, commands, weak, id) = (ui.selections.clone(), ui.commands.clone(), window.as_weak(), cfg.id);
        window.on_select(move |player| {
            selections.overlays.borrow_mut().insert(id, player);
            selections.send(&commands);
            weak.unwrap().set_selected_id(player);
        });
        let app_weak = app.as_weak();
        window.on_toggle_lock(move || {
            let app = app_weak.unwrap();
            app.set_overlays_locked(true);
            app.invoke_lock_changed(true);
        });
        ui.overlays.push(OverlayWindow { id: cfg.id, window, rows, skills, shown: false });
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
    let (ping, ping_color) = ping_text(s);
    o.window.set_ttk_text(ttk_text(s).into());
    o.window.set_ping_text(ping.into());
    o.window.set_ping_color(ping_color);
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
        .take(overlay_row_capacity(o))
        .map(|r| {
            let rate = if healing { (seconds > 0.0).then(|| r.heal as f64 / seconds) } else { r.dps };
            OverlayRow {
                id: r.id,
                name: r.name.clone().into(),
                class_icon: icons::class_icon(r.class),
                gear: gear_text(r.combat_power, r.gear_score).into(),
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

    let selected = ui.selections.of_overlay(o.id);
    o.window.set_selected_id(selected);
    if selected >= 0 {
        let name = s.rows.iter().find(|r| r.id == selected).map_or_else(|| "Skills".to_string(), |r| format!("Skills of {}", r.name));
        o.window.set_skills_title(name.into());
        sync_model(&o.skills, skill_rows(skills_of(s.skills.get(&selected), if healing { 1 } else { 0 }), seconds));
    } else {
        sync_model(&o.skills, Vec::new());
    }
}

/// How many players fit in an overlay: its height less the header, footer and padding, 24px + 4px per row. With a player's
/// skills open, at most 5 rows stay so the skills have room.
fn overlay_row_capacity(o: &OverlayWindow) -> usize {
    let height = logical_size(o.window.window()).1;
    let footer = if o.window.get_ttk_text().is_empty() && o.window.get_ping_text().is_empty() { 0.0 } else { 16.0 + 4.0 };
    let fit = (((height - 8.0 * 2.0 - 20.0 - footer) / 28.0).floor() as usize).max(1);
    if ui_selected(o) { fit.min(5) } else { fit }
}

fn ui_selected(o: &OverlayWindow) -> bool {
    o.window.get_selected_id() >= 0
}

/// Combat power and gear score beside a name: "113k · 2,410". Only party members have them (party roster).
fn gear_text(combat_power: Option<u64>, gear_score: Option<u32>) -> String {
    let power = combat_power.filter(|&p| p > 0).map(|p| {
        let p = p as f64;
        if p >= 1e6 {
            format!("{:.1}m", p / 1e6)
        } else if p >= 1e3 {
            format!("{:.0}k", p / 1e3)
        } else {
            format!("{p:.0}")
        }
    });
    let gear = gear_score.filter(|&g| g > 0).map(|g| thousands(g as u64));
    match (power, gear) {
        (Some(p), Some(g)) => format!("{p} · {g}"),
        (Some(p), None) => p,
        (None, Some(g)) => g,
        (None, None) => String::new(),
    }
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A share of `n` in `of` as "12%"; empty when `of` is 0.
fn percent(n: u32, of: u32) -> String {
    if of > 0 { format!("{:.0}%", 100.0 * n as f64 / of as f64) } else { String::new() }
}

/// The breakdown rows of `skills`; `seconds` is the time the player fought (0: unknown, no rate).
fn skill_rows(skills: &[za2_core::combat::SkillUsage], seconds: f64) -> Vec<SkillRow> {
    let total: i64 = skills.iter().map(|k| k.total).sum::<i64>().max(1);
    skills
        .iter()
        .map(|k| {
            let mut detail = vec![format!("{} - {}", compact(k.min), compact(k.max))];
            if seconds >= 1.0 {
                detail.push(format!("{}/s", compact((k.total as f64 / seconds).round() as i64)));
            }
            for (label, n) in [("back", k.back), ("perfect", k.perfect), ("double", k.double), ("parry", k.parry)] {
                if n > 0 {
                    detail.push(format!("{label} {}", percent(n, k.known_kinds)));
                }
            }
            if k.ticks > 0 {
                detail.push(format!("{} ticks", k.ticks));
            }
            SkillRow {
                icon: icons::skill_icon(k.skill_id),
                name: k.skill.clone().into(),
                total: compact(k.total).into(),
                hits: k.hits.to_string().into(),
                crit: percent(k.crit_hits, k.hits).into(),
                share: k.total as f32 / total as f32,
                detail: detail.join("  ·  ").into(),
            }
        })
        .collect()
}

/// The list of `sets` that `mode` picks: 0 damage dealt, 1 healing, 2 damage taken.
fn skills_of(sets: Option<&worker::SkillSets>, mode: i32) -> &[za2_core::combat::SkillUsage] {
    match (sets, mode) {
        (Some(s), 1) => &s.heal,
        (Some(s), 2) => &s.taken,
        (Some(s), _) => &s.damage,
        (None, _) => &[],
    }
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
    pet_overlay: PetOverlay,
    pet_overlay_rows: Rc<VecModel<PetRow>>,
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
    /// Draws the markers and spawn areas of the map and the map overlay.
    painter: map::LayerPainter,
    map: MapState,
    commands: SyncSender<WorkerCommand>,
    shared: Arc<Shared>,
    filters: Filters,
    selections: Rc<Selections>,
    /// How far the map overlay is moved away from the player, in world units.
    overlay_pan: (f64, f64),
    /// What the effect lanes were built from (the effects and the window length), so an unchanged list is not re-created
    /// on every refresh.
    lanes_from: RefCell<Option<(Vec<za2_core::buffs::BuffUptime>, u64)>>,
    /// What the Profile tab shows, so an unchanged profile is not re-created on every refresh.
    profile_shown: Option<Vec<(String, Vec<(String, String, bool)>)>>,
    /// The DPS curves the chart shows and its top value, to find the line under a click.
    chart_curves: RefCell<(Vec<(i32, Vec<f64>)>, f64)>,
}

#[derive(Default)]
struct Filters {
    resource_query: String,
    monster_query: String,
    /// The search of the map overlay.
    overlay_query: String,
    pet_query: String,
    boss_query: String,
    expanded: std::collections::BTreeSet<String>,
    /// Pets of the monster list that show their monsters.
    expanded_pets: std::collections::BTreeSet<i32>,
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

/// The names of the spawn areas that are big enough on the screen to carry one, and of the gathering spots.
fn to_labels(areas: &[map::ScreenArea], markers: &[Marker]) -> ModelRc<MapLabel> {
    let mut list: Vec<MapLabel> = areas
        .iter()
        .filter(|a| {
            let (x0, x1) = a.outline.iter().fold((f32::MAX, f32::MIN), |b, p| (b.0.min(p.0), b.1.max(p.0)));
            x1 - x0 >= 24.0
        })
        .map(|a| MapLabel { x: a.label_at.0, y: a.label_at.1, text: a.label.clone().into(), color: rgb(a.color) })
        .collect();
    list.extend(map::marker_names(markers).into_iter().map(|n| MapLabel { x: n.x, y: n.y, text: n.text.into(), color: rgb(n.color) }));
    ModelRc::new(VecModel::from(list))
}

struct ShownRow {
    id: i32,
    name: String,
    class: Option<String>,
    gear: String,
    damage: i64,
    dps: Option<f64>,
    heal: i64,
    taken: i64,
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
    let place = match &s.boss {
        Some(boss) => format!("{boss} ({})", s.zone_name),
        None => s.zone_name.clone(),
    };
    format!("{} {}  {}  {}", &start[5..10], &start[11..16], place, compact(s.total()))
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
                gear: String::new(),
                damage: r.damage,
                dps: r.dps,
                heal: r.heal,
                taken: r.taken,
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
                gear: gear_text(r.combat_power, r.gear_score),
                damage: r.damage,
                dps: r.dps,
                heal: r.heal,
                taken: r.taken,
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
            gear: r.gear.clone().into(),
            name: r.name.clone().into(),
            class_name: r.class.clone().unwrap_or_default().into(),
            class_icon: icons::class_icon(r.class.as_deref()),
            damage: compact(r.damage).into(),
            dps: r.dps.map_or("-".into(), |d| compact(d.round() as i64)).into(),
            share: r.damage as f32 / top as f32,
            share_text: if total > 0 { format!("{:.0}%", 100.0 * r.damage as f64 / total as f64) } else { String::new() }.into(),
            heal: if r.heal > 0 { compact(r.heal) } else { String::new() }.into(),
            taken: if r.taken > 0 { compact(r.taken) } else { String::new() }.into(),
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
    let (ping, ping_color) = ping_text(s);
    app.set_ttk_text(if history.is_none() { ttk_text(s) } else { String::new() }.into());
    app.set_ping_text(ping.into());
    app.set_ping_color(ping_color);

    let selected = ui.selections.meter.get();
    let mode = app.get_skill_mode();
    let player = all.iter().find(|r| r.id == selected);
    let seconds = player.and_then(|r| Some(r.damage as f64 / r.dps?)).filter(|s| s.is_finite()).unwrap_or(0.0);
    let skills = match history {
        Some(h) => h.rows.iter().find(|r| r.id == selected).map_or(&[][..], |r| match mode {
            1 => &r.heal_skills,
            2 => &r.taken_skills,
            _ => &r.skills,
        }),
        None => skills_of(s.skills.get(&selected), mode),
    };
    sync_model(&ui.models.skills, skill_rows(skills, if mode == 0 { seconds } else { 0.0 }));
    let what = ["Skills", "Healing", "Damage taken"][mode.clamp(0, 2) as usize];
    let title = match player {
        Some(r) => format!("{what} of {}", r.name),
        None if selected >= 0 => what.to_string(),
        None => "Select a player to see the skills".into(),
    };
    app.set_skills_title(title.into());
    if app.get_tab() == 2 {
        apply_timeline(app, ui);
    }
}

/// Line colours of the DPS chart, by rank.
const CHART_COLORS: [(u8, u8, u8); 8] =
    [(0xa8, 0xa0, 0xff), (0x68, 0xcb, 0xa4), (0xe5, 0xc6, 0x8a), (0xf1, 0x7b, 0x87), (0x7c, 0xc4, 0xf5), (0xf5, 0xa9, 0x7c), (0xc7, 0x92, 0xea), (0x9a, 0xa0, 0xb4)];

/// The Timeline tab: the DPS chart of the players the meter shows, their deaths, and the effects on the selected player or
/// their target. For the current fight or total, or a saved session.
fn apply_timeline(app: &App, ui: &Ui) {
    let s = &ui.snapshot;
    let history = selected_session(ui);
    let live = if ui.settings.meter_range == 0 { &s.fight_timeline } else { &s.timeline };
    let (seconds, bucket) = match history {
        Some(h) => (h.fight_seconds, h.bucket_seconds),
        None => (live.seconds, live.bucket_seconds),
    };
    let (width, height) = (app.get_chart_width(), app.get_chart_height());
    let cursor = app.get_chart_cursor();
    let shown: Vec<MeterRow> = (0..ui.models.rows.row_count()).filter_map(|i| ui.models.rows.row_data(i)).take(CHART_COLORS.len()).collect();
    let curves: Vec<(&MeterRow, Vec<f64>)> = shown
        .iter()
        .filter_map(|r| {
            let series = match history {
                Some(h) => h.rows.iter().find(|x| x.id == r.id).map(|x| &x.series),
                None => live.series.get(&r.id),
            }?;
            Some((r, chart::smoothed_dps(series, bucket)))
        })
        .collect();
    let top = chart::nice_top(curves.iter().flat_map(|c| c.1.iter().copied()).fold(0.0, f64::max));
    let lines: Vec<ChartLine> = curves
        .iter()
        .enumerate()
        .map(|(i, (r, dps))| {
            let at = (cursor >= 0.0 && !dps.is_empty()).then(|| ((cursor as f64 * (dps.len() - 1) as f64).round() as usize).min(dps.len() - 1));
            ChartLine {
                id: r.id,
                commands: chart::path_commands(dps, top, width, height).into(),
                color: rgb(CHART_COLORS[i]),
                name: r.name.clone(),
                value: at.map_or_else(|| r.dps.clone(), |k| compact(dps[k].round() as i64).into()),
                local: r.local,
            }
        })
        .collect();
    sync_model(&ui.models.chart_lines, lines);
    *ui.chart_curves.borrow_mut() = (curves.iter().map(|(r, dps)| (r.id, dps.clone())).collect(), top);
    let name_of = |id: i32| shown.iter().find(|r| r.id == id).map_or_else(|| format!("#{id}"), |r| r.name.to_string());
    let deaths: Vec<(f64, i32)> = match history {
        Some(h) => h.rows.iter().flat_map(|r| r.deaths.iter().map(move |&t| (t, r.id))).collect(),
        None => live.deaths.clone(),
    };
    let marks: Vec<ChartMark> = deaths
        .iter()
        .filter(|_| seconds > 0.0)
        .map(|&(t, id)| ChartMark { x: (t / seconds).clamp(0.0, 1.0) as f32, label: format!("{} died", name_of(id)).into() })
        .collect();
    sync_model(&ui.models.chart_marks, marks);
    app.set_chart_top(format!("{}/s", compact(top as i64)).into());
    app.set_chart_duration(duration_text(seconds).into());
    app.set_chart_cursor_time(if cursor >= 0.0 { duration_text(cursor as f64 * seconds) } else { String::new() }.into());
    app.set_chart_empty(if curves.iter().any(|c| !c.1.is_empty()) { "" } else { "No damage yet" }.into());

    let selected = ui.selections.meter.get();
    let on_target = app.get_buff_mode() == 1;
    let (title, uptimes): (String, Option<&Vec<za2_core::buffs::BuffUptime>>) = match (history, on_target) {
        (Some(h), false) => (format!("Effects on {}", name_of(selected)), h.rows.iter().find(|r| r.id == selected).map(|r| &r.buffs)),
        (Some(h), true) => match h.rows.iter().find(|r| r.id == selected).filter(|r| r.target.is_some()) {
            Some(r) => (format!("Effects on {}, {}'s main target", r.target.as_deref().unwrap_or_default(), name_of(selected)), Some(&r.target_buffs)),
            // sessions saved before za2 kept the target's effects
            None => ("Effects on the target were not saved for this session".into(), None),
        },
        (None, false) => (format!("Effects on {}", name_of(selected)), live.buffs.get(&selected)),
        (None, true) => match live.target_buffs.get(&selected) {
            Some((target, list)) => (format!("Effects on {target}, {}'s main target", name_of(selected)), Some(list)),
            None => ("Effects on the target".into(), None),
        },
    };
    let rebuild = {
        let mut last = ui.lanes_from.borrow_mut();
        let same = last.as_ref().is_some_and(|(list, at)| *at == seconds.to_bits() && list.as_slice() == uptimes.map_or(&[][..], |u| u.as_slice()));
        if !same {
            *last = Some((uptimes.cloned().unwrap_or_default(), seconds.to_bits()));
        }
        !same
    };
    let lanes: Vec<BuffLane> = uptimes
        .filter(|_| rebuild)
        .into_iter()
        .flatten()
        .take(80)
        .map(|b| BuffLane {
            icon: icons::skill_icon(b.skill.map_or(b.abnormal as i32 / 10, |k| k as i32)),
            name: b.name.clone().into(),
            uptime: if b.uptime >= 0.01 { format!("{:.0}%", 100.0 * b.uptime) } else { "<1%".into() }.into(),
            count: b.applications.to_string().into(),
            own: b.own,
            bars: ModelRc::new(VecModel::from(
                b.spans
                    .iter()
                    .filter(|_| seconds > 0.0)
                    .map(|&(a, z)| BuffBar { x: (a / seconds).clamp(0.0, 1.0) as f32, w: ((z - a) / seconds).clamp(0.0, 1.0) as f32 })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    if rebuild {
        app.set_buff_lanes(ModelRc::new(VecModel::from(lanes)));
    }
    app.set_buff_title(if selected >= 0 { title } else { "Effects".into() }.into());
    app.set_buff_empty(
        if selected < 0 { "" } else { "No timed effects" }.into(),
    );
}

/// The field bosses of a map, in the order the boss list indexes them.
fn bosses_of(map: &MapInfo) -> Vec<&'static FieldBoss> {
    za2_core::catalog::field_bosses::all().iter().filter(|b| map.table.ends_with(&format!("/{}", b.map_key))).collect()
}

/// What is known of a field boss at `now_ms`: its state ("Up", "in 1:23:45"), colour, place in the list (the ones up
/// first, then by return), and whether it is up (None: not known).
fn boss_status(board: &FieldBossBoard, boss: &FieldBoss, now_ms: i64) -> (String, Color, i64, Option<bool>) {
    let ago = |ms: i64| duration_text_long(((now_ms - ms) / 1000) as f64);
    match board.state_at(boss.world_id, now_ms) {
        Some(BossState::Up { .. }) => ("Up".to_string(), Color::from_rgb_u8(0x68, 0xcb, 0xa4), i64::MIN, Some(true)),
        Some(BossState::Down { back_ms }) if back_ms > 0 => {
            (format!("in {}", duration_text_long(((back_ms - now_ms) / 1000) as f64)), Color::from_rgb_u8(0xe5, 0xc6, 0x8a), back_ms, Some(false))
        }
        Some(BossState::Down { .. }) => ("Down".to_string(), Color::from_rgb_u8(0xf1, 0x7b, 0x87), i64::MAX - 1, Some(false)),
        Some(BossState::Killed { at_ms }) => (format!("killed {} ago", ago(at_ms)), Color::from_rgb_u8(0xf1, 0x7b, 0x87), i64::MAX - 1, Some(false)),
        None => ("?".to_string(), Color::from_rgb_u8(0x96, 0x98, 0xa8), i64::MAX, None),
    }
}

/// The field bosses of the shown map, with what is known of them.
fn apply_bosses(app: &App, ui: &Ui) {
    let map = displayed_map(ui);
    let now_ms = ui.snapshot.now / 1000;
    let board = &ui.snapshot.field_bosses;
    let bosses = bosses_of(map);
    let ago = |ms: i64| duration_text_long(((now_ms - ms) / 1000) as f64);
    let query = ui.filters.boss_query.trim();
    let mut rows: Vec<(i64, BossRow)> = bosses
        .iter()
        .enumerate()
        .filter(|(_, b)| matches_query(&b.name, query))
        .map(|(index, b)| {
            let seen = board.bosses.get(&b.world_id).map(|s| s.seen_ms);
            let (status, color, order, _) = boss_status(board, b, now_ms);
            let detail = seen.map_or_else(|| "not seen yet".to_string(), |ms| format!("seen {} ago", ago(ms)));
            let shown = ui.settings.shown_bosses.contains(&b.world_id);
            (order, BossRow { name: b.name.clone().into(), status: status.into(), detail: detail.into(), color, index: index as i32, shown })
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    let summary = match (bosses.is_empty(), rows.is_empty()) {
        (true, _) => "No field bosses on this map.",
        (false, true) => "No field boss matches.",
        (false, false) => "",
    };
    sync_model(&ui.models.boss_rows, rows.into_iter().map(|r| r.1).collect());
    app.set_boss_summary(summary.into());
}

/// The field bosses ticked in the boss list that are in view: their markers, a fingerprint of them (the markers are redrawn
/// when it changes), and a label under each one that is not up, with its name and when it is back.
fn boss_marks(ui: &Ui, map: &MapInfo, view: &View) -> (Vec<Marker>, u64, Vec<MapLabel>) {
    use std::hash::{Hash, Hasher};
    let now_ms = ui.snapshot.now / 1000;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let (mut markers, mut labels) = (Vec::new(), Vec::new());
    for b in bosses_of(map).into_iter().filter(|b| ui.settings.shown_bosses.contains(&b.world_id)) {
        let (status, color, _, up) = boss_status(&ui.snapshot.field_bosses, b, now_ms);
        let rgb8 = (color.red(), color.green(), color.blue());
        let Some(marker) = map::boss_marker(map, view, b.npc, b.x, b.y, rgb8) else { continue };
        (b.world_id, rgb8).hash(&mut hash);
        let text = match up {
            Some(false) => format!("{} · {status}", b.name),
            _ => b.name.clone(),
        };
        labels.push(MapLabel { x: marker.x, y: marker.y + marker.size / 2.0, text: text.into(), color });
        markers.push(marker);
    }
    (markers, hash.finish(), labels)
}

/// Most field bosses pinned at once, the soonest back first.
const MAX_BOSS_PINS: usize = 6;

/// The shown field bosses whose marker is not on the map (out of view, or on another map) as cards, the ones up first,
/// then by return.
fn boss_pins(ui: &Ui, map: &MapInfo, view: &View) -> Vec<BossPin> {
    if !ui.settings.boss_pins {
        return Vec::new();
    }
    let now_ms = ui.snapshot.now / 1000;
    let mut pins: Vec<(i64, BossPin)> = za2_core::catalog::field_bosses::all()
        .iter()
        .filter(|b| ui.settings.shown_bosses.contains(&b.world_id))
        .filter(|b| !map.table.ends_with(&format!("/{}", b.map_key)) || map::boss_marker(map, view, b.npc, b.x, b.y, (0, 0, 0)).is_none())
        .map(|b| {
            let (status, color, order, _) = boss_status(&ui.snapshot.field_bosses, b, now_ms);
            let icon = map::monster_icon(b.npc);
            let pin = BossPin {
                icon: icon.map(icons::load).unwrap_or_default(),
                glyph: icon.is_none(),
                name: b.name.clone().into(),
                status: status.into(),
                detail: b.map_name.clone().into(),
                color,
                world_id: b.world_id as i32,
            };
            (order, pin)
        })
        .collect();
    pins.sort_by_key(|p| p.0);
    pins.into_iter().take(MAX_BOSS_PINS).map(|p| p.1).collect()
}

/// The Profile tab: what is known of the own character, its pets and the connection.
fn apply_profile(app: &App, ui: &mut Ui) {
    let s = &ui.snapshot;
    let unknown = "—".to_string();
    let or_unknown = |v: Option<String>| v.unwrap_or_else(|| unknown.clone());
    let own = s.own.as_ref();
    let mut sections: Vec<(String, Vec<(String, String, bool)>)> = Vec::new();
    let line = |label: &str, value: String| (label.to_string(), value, false);

    let logged_in = own.and_then(|o| o.logged_in_at).filter(|&at| at > 0 && s.now >= at).map(|at| format!("{} ago", duration_text_long(((s.now - at) / 1_000_000) as f64)));
    sections.push((
        "CHARACTER".into(),
        vec![
            line("Name", or_unknown(own.map(|o| o.name.clone()).or_else(|| s.learned_name.clone()))),
            line("Class", or_unknown(own.and_then(|o| o.class).map(str::to_string))),
            line("Level", or_unknown(own.and_then(|o| o.level).map(|l| l.to_string()))),
            line("Faction", or_unknown(s.faction.map(str::to_string))),
            line("Server", or_unknown(own.and_then(|o| o.server_id).map(|id| id.to_string()))),
            line("Legion", or_unknown(own.and_then(|o| o.guild.clone()))),
            line("Combat power", own.and_then(|o| o.combat_power).map_or_else(|| "— (known while in a party)".to_string(), thousands)),
            line("Gear score", own.and_then(|o| o.gear_score).map_or_else(|| "— (known while in a party)".to_string(), |g| thousands(g as u64))),
            line("Logged in", or_unknown(logged_in)),
        ],
    ));
    let (basic, charged) = s.odyle.map_or((unknown.clone(), unknown.clone()), |(b, c)| (thousands(b as u64), thousands(c as u64)));
    sections.push(("ODYLE ENERGY".into(), vec![("Odyle energy".into(), basic, true), ("Charged".into(), charged, true)]));

    let (source, _, _) = pet_source_text(s.pet_source);
    let all = pets::all().len();
    let unlocked = s.pets.unlocked();
    let maxed = s.pets.maxed();
    let levels: u32 = pets::all().into_iter().map(|p| s.pets.get(p).level as u32).sum();
    sections.push((
        "PETS".into(),
        vec![
            line("Unlocked", format!("{unlocked} of {all}")),
            line("At max level", format!("{maxed} of {all}")),
            line("Still to level", (unlocked - maxed.min(unlocked)).to_string()),
            line("Locked", (all - unlocked.min(all)).to_string()),
            line("Levels in total", levels.to_string()),
            line("Pinned to the overlay", ui.settings.pinned_pets.len().to_string()),
            line("Source", source.to_string()),
        ],
    ));

    let position = s.position.as_ref();
    let (ping, _) = ping_text(s);
    sections.push((
        "WORLD AND CONNECTION".into(),
        vec![
            line("Zone", if s.zone_name.is_empty() { unknown.clone() } else { s.zone_name.clone() }),
            line("Map", or_unknown(position.and_then(|p| p.map).map(|m| m.title.clone()))),
            line(
                "Position",
                position
                    .filter(|p| p.state != PositionState::Unknown)
                    .map_or_else(|| state_text(PositionState::Unknown).to_string(), |p| format!("{:.0}, {:.0}  ·  {}", p.x / 100.0, p.y / 100.0, state_text(p.state))),
            ),
            line("Ping", if ping.is_empty() { unknown.clone() } else { ping }),
            line("Connection", s.status.clone()),
            line("Memory reader", if s.memory_status.is_empty() { unknown.clone() } else { s.memory_status.clone() }),
            line("Saved boss fights", s.sessions.len().to_string()),
        ],
    ));

    if ui.profile_shown.as_ref() == Some(&sections) {
        return;
    }
    let model: Vec<ProfileSection> = sections
        .iter()
        .map(|(title, items)| ProfileSection {
            title: title.clone().into(),
            items: ModelRc::new(VecModel::from(
                items.iter().map(|(label, value, accent)| ProfileItem { label: label.clone().into(), value: value.clone().into(), accent: *accent }).collect::<Vec<_>>(),
            )),
        })
        .collect();
    app.set_profile(ModelRc::new(VecModel::from(model)));
    app.set_profile_empty(SharedString::new());
    ui.profile_shown = Some(sections);
}

/// "1:23:45" for an hour or more, else "23:45".
/// The wall clock in unix milliseconds.
fn wall_clock_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The Timers page, by the wall clock (the timetable is not the capture's).
fn apply_timers(app: &App) {
    let now = wall_clock_ms();
    app.set_timer_rows(ModelRc::new(VecModel::from(timers::rows(now))));
    app.set_timer_clock(timers::clock(now).into());
}

fn duration_text_long(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { duration_text(s as f64) }
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
fn monster_name(npc: i32) -> String {
    za2_core::catalog::npcs::name_of(npc, "en").map_or_else(|| format!("#{npc}"), str::to_string)
}

fn pet_label(pet: i32) -> String {
    pets::pet_name(pet, "en").map_or_else(|| format!("Pet #{pet}"), str::to_string)
}

/// A pet with its level and the kills to the next one.
fn pet_row(pet: i32, state: PetState, pinned: bool, target: bool) -> PetRow {
    let max = state.is_max();
    let needed = pet_progress::needed(state.level);
    PetRow {
        id: pet,
        name: pet_label(pet).into(),
        species: pets::species_of(pet).unwrap_or_default().into(),
        icon: pets::icon_of(pet).map(icons::load).unwrap_or_default(),
        level: state.level as i32,
        progress: if max { "Max".into() } else { format!("{}/{needed}", state.progress).into() },
        left: if max { "Max".into() } else { format!("{} left", state.left()).into() },
        to_max: if max || state.level + 1 == pet_progress::MAX_LEVEL { String::new() } else { format!("{} to max", state.left_to_max()) }.into(),
        share: if max { 1.0 } else { state.progress as f32 / needed as f32 },
        pinned,
        target,
        max,
    }
}

/// Where the pets come from, for the Pets tab and the overlay header.
fn pet_source_text(source: PetSource) -> (&'static str, &'static str, Color) {
    match source {
        PetSource::Memory => ("Live from the game", "live", Color::from_rgb_u8(0x68, 0xcb, 0xa4)),
        PetSource::Packets => ("From the game traffic", "traffic", Color::from_rgb_u8(0xa8, 0xa0, 0xff)),
        PetSource::Saved => ("Saved from the last session; exact again at the next login", "saved", Color::from_rgb_u8(0xe5, 0xc6, 0x8a)),
        PetSource::None => ("Not known yet: needs the memory reader, or a login while za2 runs", "unknown", Color::from_rgb_u8(0xf1, 0x7b, 0x87)),
    }
}

/// Every pet of the pet window that matches the search and the filter (0 all, 1 pinned, 2 not at max).
fn apply_pets(app: &App, ui: &Ui) {
    let s = &ui.snapshot;
    let query = ui.filters.pet_query.trim();
    let filter = app.get_pet_filter();
    let species = pet_species();
    let wanted_species = usize::try_from(app.get_pet_species() - 1).ok().and_then(|i| species.get(i));
    let mut rows: Vec<(PetState, PetRow)> = pets::all()
        .into_iter()
        .filter(|&p| wanted_species.is_none_or(|w| pets::species_of(p) == Some(w.as_str())))
        .filter(|&p| matches_query(&pet_label(p), query) || pets::species_of(p).is_some_and(|sp| matches_query(sp, query)))
        .map(|p| (s.pets.get(p), pet_row(p, s.pets.get(p), ui.settings.pinned_pets.contains(&p), s.target_pets.contains(&p))))
        .filter(|(_, r)| match filter {
            1 => r.pinned,
            2 => !r.max,
            _ => true,
        })
        .collect();
    sort_pets(&mut rows, app.get_pet_sort(), app.get_pet_sort_desc());
    sync_model(&ui.models.pet_rows, rows.into_iter().map(|r| r.1).collect());
    let (source, _, color) = pet_source_text(s.pet_source);
    app.set_pet_source(source.into());
    app.set_pet_source_color(color);
    let all = pets::all().len();
    let summary = format!(
        "·  {} of {all} unlocked  ·  {} at max  ·  {} pinned",
        s.pets.unlocked(),
        s.pets.maxed(),
        ui.settings.pinned_pets.len()
    );
    app.set_pet_summary(summary.into());
}

/// The pet types of the pet window (Cognia, Fera, ...), by name.
fn pet_species() -> Vec<String> {
    let mut list: Vec<String> = pets::all().into_iter().filter_map(pets::species_of).map(str::to_string).collect();
    list.sort();
    list.dedup();
    list
}

/// Orders the Pets tab by a column (0 name, 1 level, 2 progress to the next level, 3 kills left), the pinned pets first so
/// they are quick to unpin. Equal values go by name.
fn sort_pets(rows: &mut [(PetState, PetRow)], column: i32, descending: bool) {
    rows.sort_by(|(a, ra), (b, rb)| {
        let by_column = match column {
            1 => (a.level, a.progress).cmp(&(b.level, b.progress)),
            2 => ra.share.total_cmp(&rb.share),
            // a pet at max has nothing left: after all the others
            3 => (a.is_max(), a.left()).cmp(&(b.is_max(), b.left())),
            _ => std::cmp::Ordering::Equal,
        };
        let by_name = ra.name.to_lowercase().cmp(&rb.name.to_lowercase());
        let order = if column == 0 { by_name } else { by_column.then(by_name) };
        rb.pinned.cmp(&ra.pinned).then(if descending { order.reverse() } else { order })
    });
}

/// The pet overlay: the pet of the monster the player fights first (when it is not pinned), then the pinned pets by name.
/// Its height follows the rows; the width is the user's.
fn apply_pet_overlay(ui: &Ui) {
    let s = &ui.snapshot;
    let pinned = &ui.settings.pinned_pets;
    let mut ids: Vec<i32> = if ui.settings.pet_overlay_target { s.target_pets.iter().copied().filter(|p| !pinned.contains(p)).collect() } else { Vec::new() };
    let mut by_name: Vec<i32> = pinned.iter().copied().collect();
    by_name.sort_by_key(|&p| pet_label(p).to_lowercase());
    ids.extend(by_name);
    let rows: Vec<PetRow> = ids.into_iter().map(|p| pet_row(p, s.pets.get(p), pinned.contains(&p), s.target_pets.contains(&p))).collect();
    let count = rows.len();
    sync_model(&ui.pet_overlay_rows, rows);
    let (_, source, _) = pet_source_text(s.pet_source);
    let o = &ui.pet_overlay;
    o.set_header(format!("Pets  ·  {source}").into());
    let width = ui.settings.pet_overlay_size.map_or(340.0, |s| s.0);
    let height = 8.0 * 2.0 + 20.0 + 4.0 + count.max(1) as f32 * 30.0;
    if logical_size(o.window()) != (width, height) {
        o.window().set_size(slint::LogicalSize::new(width, height));
    }
}

/// " · Elite" for the ranks above normal.
fn rank_suffix(npc: i32) -> &'static str {
    match za2_core::catalog::monsters::info(npc).map(|m| m.rank.as_str()) {
        Some("Elite") => " · Elite",
        Some("Hero") => " · Hero",
        Some("Legend") => " · Legend",
        _ => "",
    }
}

/// The monsters of a map, grouped: each pet with the monsters that give it (sorted by pet name), then the monsters that give
/// no pet. Only what matches `query` (a pet matches by its own name or a monster's; then its matching monsters are shown).
fn monster_groups(map: &MapInfo, query: &str) -> (Vec<(i32, Vec<i32>)>, Vec<i32>) {
    let on_map = za2_core::catalog::monsters::on_map(&map.table);
    let by_name = |a: &i32, b: &i32| monster_name(*a).to_lowercase().cmp(&monster_name(*b).to_lowercase());
    let mut groups: Vec<(i32, Vec<i32>)> = Vec::new();
    let mut alone: Vec<i32> = Vec::new();
    for &npc in &on_map {
        let pets_of = pets::pets_of_npc(npc);
        if pets_of.is_empty() {
            if matches_query(&monster_name(npc), query) {
                alone.push(npc);
            }
            continue;
        }
        for &pet in pets_of {
            if !(matches_query(&pet_label(pet), query) || matches_query(&monster_name(npc), query)) {
                continue;
            }
            match groups.iter_mut().find(|g| g.0 == pet) {
                Some(g) => g.1.push(npc),
                None => groups.push((pet, vec![npc])),
            }
        }
    }
    for g in &mut groups {
        g.1.sort_by(by_name);
    }
    groups.sort_by(|a, b| pet_label(a.0).to_lowercase().cmp(&pet_label(b.0).to_lowercase()));
    alone.sort_by(by_name);
    (groups, alone)
}

/// Ticks a pet (all its monsters) or unticks it with its monsters.
fn toggle_pet(settings: &mut Settings, pet: i32, on: bool) {
    let npcs = pets::npcs_of_pet(pet);
    settings.monsters.retain(|n| !npcs.contains(n));
    if on {
        settings.pets.insert(pet);
    } else {
        settings.pets.remove(&pet);
    }
}

/// Ticks one monster, or unticks it; unticking a monster of a ticked pet keeps the pet's other monsters ticked.
fn toggle_monster(settings: &mut Settings, npc: i32, on: bool) {
    if on {
        settings.monsters.insert(npc);
        return;
    }
    settings.monsters.remove(&npc);
    for pet in pets::pets_of_npc(npc) {
        if settings.pets.remove(pet) {
            settings.monsters.extend(pets::npcs_of_pet(*pet).iter().filter(|&&n| n != npc));
        }
    }
}

/// The pets the map has ticked: the ticked pets and the pets of the ticked monsters.
fn ticked_pets(settings: &Settings) -> BTreeSet<i32> {
    settings.pets.iter().copied().chain(settings.monsters.iter().flat_map(|&n| pets::pets_of_npc(n).iter().copied())).collect()
}

/// With the sync on, the pinned pets are the pets ticked on the map: ticking pins, unticking unpins.
fn sync_pins(settings: &mut Settings) {
    if settings.pet_auto_pin {
        settings.pinned_pets = ticked_pets(settings);
    }
}

/// Pins or unpins a pet; with the sync on, also ticks or unticks it (with its monsters) on the map.
fn set_pin(settings: &mut Settings, pet: i32, on: bool) {
    if on {
        settings.pinned_pets.insert(pet);
    } else {
        settings.pinned_pets.remove(&pet);
    }
    if settings.pet_auto_pin {
        toggle_pet(settings, pet, on);
    }
}

/// Turning the sync on: the pets pinned by hand are ticked on the map, then the pins follow the map.
fn start_pin_sync(settings: &mut Settings) {
    if !settings.pet_auto_pin {
        return;
    }
    let ticked = ticked_pets(settings);
    for pet in settings.pinned_pets.clone() {
        if !ticked.contains(&pet) {
            toggle_pet(settings, pet, true);
        }
    }
    sync_pins(settings);
}

/// After the ticked pets or monsters changed: the pins, the lists and the maps.
fn monsters_changed(app: &App, u: &mut Ui) {
    sync_pins(&mut u.settings);
    u.settings.save();
    apply_filters(app, u);
    apply_map(app, u, true);
    apply_pets(app, u);
    if u.settings.pet_overlay {
        apply_pet_overlay(u);
    }
}

fn monster_spawn_count(map: &MapInfo, npc: i32) -> usize {
    za2_core::catalog::monsters::spawns_of(&map.table, npc).len()
}

/// The map the player stands on, else the one the app shows.
fn player_map(ui: &Ui) -> &'static MapInfo {
    ui.snapshot.position.as_ref().and_then(|p| p.map).unwrap_or_else(|| displayed_map(ui))
}

/// Puts the ticked rows first (so they are quick to untick), keeping the order within the ticked and the others.
fn ticked_first(rows: &mut [FilterRow]) {
    rows.sort_by_key(|r| !(r.checked || r.partial));
}

/// The same for lists of groups (a header with the rows under it): the groups with something ticked first.
fn ticked_groups_first(groups: Vec<Vec<FilterRow>>) -> Vec<FilterRow> {
    let mut groups = groups;
    groups.sort_by_key(|g| !g.first().is_some_and(|r| r.checked || r.partial));
    groups.into_iter().flatten().collect()
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
                icon: map_icons::gather(&item.kind).map(icons::load).unwrap_or_default(),
                ..Default::default()
            });
        }
    }
    let colors: HashMap<i32, (u8, u8, u8)> = map::monster_colors(&map.table, &ui.settings.pets, &ui.settings.monsters).into_iter().collect();
    let (groups, _) = monster_groups(map, query);
    for (pet, _) in &groups {
        let on = ui.settings.pets.contains(pet);
        let icon = pets::icon_of(*pet).map(icons::load).unwrap_or_default();
        let color = pets::npcs_of_pet(*pet).iter().find_map(|n| colors.get(n)).copied().unwrap_or((0xE0, 0x50, 0x50));
        rows.push(FilterRow { key: format!("p:{pet}").into(), label: pet_label(*pet).into(), detail: "pet".into(), checked: on, color: rgb(color), icon, ..Default::default() });
    }
    // every monster of the map by its own name (also those that give a pet)
    if !query.is_empty() {
        for npc in za2_core::catalog::monsters::on_map(&map.table).into_iter().filter(|n| matches_query(&monster_name(*n), query)) {
            let on = ui.settings.monsters.contains(&npc);
            let icon = map::monster_icon(npc).map(icons::load).unwrap_or_default();
            let glyph = map::monster_icon(npc).is_none();
            let color = colors.get(&npc).copied().unwrap_or((0xE0, 0x50, 0x50));
            let detail = format!("monster{}", rank_suffix(npc));
            rows.push(FilterRow { key: format!("m:{npc}").into(), label: monster_name(npc).into(), detail: detail.into(), checked: on, color: rgb(color), icon, glyph, ..Default::default() });
        }
    }
    ticked_first(&mut rows);
    rows.truncate(300);
    overlay.set_search_rows(ModelRc::new(VecModel::from(rows)));
}

fn apply_filters(app: &App, ui: &Ui) {
    let map = displayed_map(ui);
    let query = ui.filters.resource_query.trim();
    let selected = &ui.settings.gather_items;
    let mut blocks: Vec<Vec<FilterRow>> = Vec::new();
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
        let icon = map_icons::gather(kind).map(icons::load).unwrap_or_default();
        let mut rows: Vec<FilterRow> = Vec::new();
        rows.push(FilterRow {
            key: (*kind).into(),
            label: map::kind_label(kind).into(),
            detail: places.to_string().into(),
            header: true,
            expanded,
            checked: picked == shown.len(),
            partial: picked > 0 && picked < shown.len(),
            color: rgb(map::gather_color(kind)),
            icon: icon.clone(),
            glyph: false,
            child: false,
        });
        if expanded {
            let mut shown = shown;
            shown.sort_by_key(|i| !selected.contains(&i.key));
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
                    icon: icon.clone(),
                    glyph: false,
                    child: true,
                });
            }
        }
        blocks.push(rows);
    }
    let rows = ticked_groups_first(blocks);
    let summary = match (rows.is_empty(), chosen) {
        (true, _) => "nothing matches".to_string(),
        (false, 0) => String::new(),
        (false, n) => format!("{n} selected"),
    };
    app.set_resource_rows(ModelRc::new(VecModel::from(rows)));
    app.set_resource_summary(summary.into());

    // Pets with the monsters that give them, then the other monsters; colours as on the map (map::monster_colors).
    let query = ui.filters.monster_query.trim();
    let colors: HashMap<i32, (u8, u8, u8)> = map::monster_colors(&map.table, &ui.settings.pets, &ui.settings.monsters).into_iter().collect();
    let (groups, alone) = monster_groups(map, query);
    let monster_row = |npc: i32, checked: bool, child: bool| FilterRow {
        key: npc.to_string().into(),
        label: monster_name(npc).into(),
        detail: format!("{} spawns{}", monster_spawn_count(map, npc), rank_suffix(npc)).into(),
        checked,
        color: rgb(colors.get(&npc).copied().unwrap_or((255, 255, 255))),
        icon: map::monster_icon(npc).map(icons::load).unwrap_or_default(),
        glyph: map::monster_icon(npc).is_none(),
        child,
        ..Default::default()
    };
    let mut blocks: Vec<Vec<FilterRow>> = Vec::new();
    for (pet, npcs) in &groups {
        let pet_on = ui.settings.pets.contains(pet);
        let some = npcs.iter().any(|n| ui.settings.monsters.contains(n));
        let expanded = !query.is_empty() || ui.filters.expanded_pets.contains(pet);
        let spawns: usize = npcs.iter().map(|&n| monster_spawn_count(map, n)).sum();
        let mut monster_rows: Vec<FilterRow> = Vec::new();
        monster_rows.push(FilterRow {
            key: pet.to_string().into(),
            label: pet_label(*pet).into(),
            detail: format!("{} monster{} · {spawns} spawns", npcs.len(), if npcs.len() == 1 { "" } else { "s" }).into(),
            header: true,
            expanded,
            checked: pet_on,
            partial: !pet_on && some,
            color: rgb(npcs.iter().find_map(|n| colors.get(n)).copied().unwrap_or((255, 255, 255))),
            icon: pets::icon_of(*pet).map(icons::load).unwrap_or_default(),
            glyph: false,
            child: false,
        });
        if expanded {
            let mut children: Vec<FilterRow> = npcs.iter().map(|&n| monster_row(n, pet_on || ui.settings.monsters.contains(&n), true)).collect();
            ticked_first(&mut children);
            monster_rows.extend(children);
        }
        blocks.push(monster_rows);
    }
    blocks.extend(alone.iter().map(|&n| vec![monster_row(n, ui.settings.monsters.contains(&n), false)]));
    let monster_rows = ticked_groups_first(blocks);
    let shown_monsters = groups.iter().map(|g| g.1.len()).sum::<usize>() + alone.len();
    let summary = format!("{} pets · {shown_monsters} monsters  ·  {} marked", groups.len(), colors.len());
    app.set_monster_rows(ModelRc::new(VecModel::from(monster_rows)));
    app.set_monster_summary(summary.into());
}

/// A fingerprint of the live inputs the markers depend on, to the metre: equal fingerprints draw the same markers.
fn scene_key(live: &LiveInputs) -> u64 {
    use std::hash::{Hash, Hasher};
    let metre = |v: f32| (v / 100.0).round() as i64;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (live.known, metre(live.player.0), metre(live.player.1), live.player_z.map(metre), live.show_live_mobs, live.show_cubes).hash(&mut h);
    for e in live.env {
        (&e.key, &e.kind, metre(e.x), metre(e.y), metre(e.z)).hash(&mut h);
    }
    if live.show_live_mobs {
        for m in live.mobs {
            (m.entity, m.npc, metre(m.x), metre(m.y), metre(m.z)).hash(&mut h);
        }
    }
    h.finish()
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
    let view = View { center_x: ui.map.center.0, center_y: ui.map.center.1, radius_metres: ui.settings.map_radius_metres, width, height };
    let known = tracked_here && position.as_ref().is_some_and(|p| matches!(p.state, PositionState::Exact | PositionState::Estimated));
    let live = LiveInputs {
        known,
        player: position.as_ref().map_or((0.0, 0.0), |p| (p.x, p.y)),
        player_z: position.as_ref().and_then(|p| p.z),
        env: &ui.snapshot.live_env,
        mobs: &ui.snapshot.live_mobs,
        show_live_mobs: ui.settings.show_live_mobs,
        show_cubes: ui.settings.show_cubes,
    };
    let (bosses, bosses_key, boss_labels) = boss_marks(ui, map, &view);
    sync_model(&ui.models.boss_labels, boss_labels);
    sync_model(&ui.models.boss_pins, boss_pins(ui, map, &view));
    let key = (
        map.key.clone(),
        (ui.map.center.0.round() as i64, ui.map.center.1.round() as i64),
        (ui.settings.map_radius_metres * 10.0) as i64,
        (width as i64, height as i64),
        scene_key(&live) ^ bosses_key,
    );
    // Tiles and markers are rebuilt only when what they show changed: a new model re-creates every marker element.
    if force || ui.map.last_built.as_ref() != Some(&key) {
        ui.map.last_built = Some(key);
        let faction = faction_of(&ui.snapshot);
        ui.map.layer.update(Some(map), &ui.settings.gather_items, &ui.settings.pets, &ui.settings.monsters, faction);
        let quads = map::visible_tiles(map, &view, &mut ui.loader);
        map::markers(map, &view, &ui.map.layer, &live, &mut ui.map.markers);
        ui.map.markers.extend(bosses);
        let areas = map::screen_areas(map, &view, &ui.map.layer);
        let scale = app.window().scale_factor();
        app.set_tiles(to_tiles(quads));
        app.set_layer(ui.painter.paint(width, height, scale, &ui.map.markers, &areas));
        app.set_labels(to_labels(&areas, &ui.map.markers));
    }

    match position.as_ref().filter(|_| tracked_here) {
        Some(p) => {
            let (x, y) = view.to_screen(map, p.x as f64, p.y as f64);
            app.set_me_visible(true);
            app.set_me_x(x as f32);
            app.set_me_y(y as f32);
            app.set_me_heading(p.heading as f32);
            app.set_me_color(state_color(p.state));
            app.set_me_label(if p.state == PositionState::Stale { "?".into() } else { SharedString::new() });
            app.set_radar_radius(radar_radius(&ui.settings, map, &view, p.x as f64, p.y as f64));
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
    let mut elsewhere = false;
    if let Some(p) = position.as_ref() {
        if let Some(m) = p.map {
            if m != map {
                elsewhere = true;
                text.push_str(&format!(" on {}", m.title));
            }
        } else if state != PositionState::Unknown {
            elsewhere = true;
            text.push_str(", map not detected");
        }
    }
    // an exact position on the shown map needs no badge
    if state == PositionState::Exact && !elsewhere {
        text.clear();
    }
    app.set_position_text(text.into());
    app.set_position_color(state_color(state));
    let index = maps::all().iter().position(|m| m == map).unwrap_or(0) as i32;
    if app.get_map_index() != index {
        app.set_map_index(index);
    }

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
        overlay.set_layer(slint::Image::default());
        overlay.set_labels(ModelRc::new(VecModel::from(Vec::<MapLabel>::new())));
        sync_model(&ui.models.overlay_boss_labels, Vec::new());
        overlay.set_me_visible(false);
        return;
    };
    let width = overlay.get_view_width() as f64;
    let height = overlay.get_view_height() as f64;
    if width < 10.0 || height < 10.0 {
        return;
    }
    let center = (p.x as f64 + ui.overlay_pan.0, p.y as f64 + ui.overlay_pan.1);
    let view = View { center_x: center.0, center_y: center.1, radius_metres: ui.settings.overlay_radius_metres, width, height };
    let live = LiveInputs {
        known: matches!(p.state, PositionState::Exact | PositionState::Estimated),
        player: (p.x, p.y),
        player_z: p.z,
        env: &ui.snapshot.live_env,
        mobs: &ui.snapshot.live_mobs,
        show_live_mobs: ui.settings.show_live_mobs,
        show_cubes: ui.settings.show_cubes,
    };
    let (bosses, bosses_key, boss_labels) = boss_marks(ui, map, &view);
    sync_model(&ui.models.overlay_boss_labels, boss_labels);
    let key = (map.key.clone(), (center.0 as i64, center.1 as i64), (width as i64, height as i64), scene_key(&live) ^ bosses_key);
    if force || ui.map.last_overlay.as_ref() != Some(&key) {
        ui.map.last_overlay = Some(key);
        ui.map.overlay_layer.update(Some(map), &ui.settings.gather_items, &ui.settings.pets, &ui.settings.monsters, faction_of(&ui.snapshot));
        let quads = map::visible_tiles(map, &view, &mut ui.loader);
        let mut markers = Vec::new();
        map::markers(map, &view, &ui.map.overlay_layer, &live, &mut markers);
        markers.extend(bosses);
        let areas = map::screen_areas(map, &view, &ui.map.overlay_layer);
        let scale = overlay.window().scale_factor();
        overlay.set_tiles(to_tiles(quads));
        overlay.set_layer(ui.painter.paint(width, height, scale, &markers, &areas));
        overlay.set_labels(to_labels(&areas, &markers));
    }
    let (me_x, me_y) = view.to_screen(map, p.x as f64, p.y as f64);
    overlay.set_me_visible(p.state != PositionState::Unknown);
    overlay.set_me_x(me_x as f32);
    overlay.set_me_y(me_y as f32);
    overlay.set_radar_radius(radar_radius(&ui.settings, map, &view, p.x as f64, p.y as f64));
    overlay.set_panned(ui.overlay_pan != (0.0, 0.0));
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

/// Saves the sizes of the overlays the user resized, so they open at that size next time.
fn remember_overlay_sizes(ui: &mut Ui, map_overlay: &MapOverlay) {
    let mut changed = false;
    let sizes: Vec<(u32, (f32, f32))> = ui.overlays.iter().filter(|o| o.shown).map(|o| (o.id, logical_size(o.window.window()))).collect();
    for (id, size) in sizes {
        if let Some(cfg) = ui.settings.overlays.iter_mut().find(|c| c.id == id) {
            if size.0 >= 1.0 && cfg.size != Some(size) {
                cfg.size = Some(size);
                changed = true;
            }
        }
    }
    if ui.settings.pet_overlay && ui.pet_overlay.window().is_visible() {
        let size = logical_size(ui.pet_overlay.window());
        if size.0 >= 1.0 && ui.settings.pet_overlay_size.map(|s| s.0) != Some(size.0) {
            ui.settings.pet_overlay_size = Some(size);
            changed = true;
        }
    }
    if ui.settings.map_overlay && map_overlay.window().is_visible() {
        let size = logical_size(map_overlay.window());
        if size.0 >= 1.0 && ui.settings.map_overlay_size != Some(size) {
            ui.settings.map_overlay_size = Some(size);
            changed = true;
        }
    }
    if changed {
        ui.settings.save();
    }
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

/// Screen radius of the Hidden Cube radar circle around (x, y); 0 when the Hidden Cubes are hidden.
fn radar_radius(settings: &Settings, map: &MapInfo, view: &View, x: f64, y: f64) -> f32 {
    if !settings.show_cubes {
        return 0.0;
    }
    let (cx, cy) = view.to_screen(map, x, y);
    let (ex, ey) = view.to_screen(map, x + map::CUBE_RADAR_RANGE, y);
    (ex - cx).hypot(ey - cy) as f32
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
    let mut settings = Settings::load();
    // the overlays start unlocked, so they can be moved; Ctrl+L or their lock button locks them
    settings.overlays_locked = false;
    start_pin_sync(&mut settings);
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
    // Wayland compositors find the icon through the desktop entry named after the app id (see desktop.rs); without
    // one they show a generic icon in the task switcher. Set before any window is shown.
    let _ = slint::set_xdg_app_id("za2");
    let map_overlay = MapOverlay::new().expect("cannot create the map overlay");
    let pet_overlay = PetOverlay::new().expect("cannot create the pet overlay");
    let pet_overlay_rows = Rc::new(VecModel::default());
    pet_overlay.set_rows(ModelRc::from(pet_overlay_rows.clone()));
    pet_overlay.set_empty_text("Pin pets in the Pets tab of za2.".into());
    pet_overlay.set_locked(settings.overlays_locked);
    pet_overlay.set_overlay_opacity(settings.overlay_opacity);

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
        painter: map::LayerPainter::default(),
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
        pet_overlay: pet_overlay.clone_strong(),
        pet_overlay_rows,
        sound_test: None,
        selections: Rc::new(Selections::new()),
        overlay_pan: (0.0, 0.0),
        lanes_from: RefCell::new(None),
        profile_shown: None,
        chart_curves: RefCell::new((Vec::new(), 1.0)),
    }));
    map_overlay.set_boss_labels(ModelRc::from(ui.borrow().models.overlay_boss_labels.clone()));

    app.set_map_names(ModelRc::new(VecModel::from(maps::all().iter().map(|m| SharedString::from(m.title.as_str())).collect::<Vec<_>>())));
    app.set_own_name(settings.own_character_name.clone().into());
    app.set_reset_on_zone(settings.reset_on_zone_change);
    app.set_scope(settings.scope);
    app.set_show_live_mobs(settings.show_live_mobs);
    app.set_show_cubes(settings.show_cubes);
    app.set_cube_icon(map_icons::gather("Cube").map(icons::load).unwrap_or_default());
    app.set_cube_sound(settings.cube_sound_enabled);
    app.set_cube_volume(settings.cube_sound_volume as f32);
    app.set_meter_overlay(settings.meter_overlay);
    app.set_map_overlay(settings.map_overlay);
    app.set_overlays_locked(settings.overlays_locked);
    app.set_overlay_opacity(settings.overlay_opacity);
    app.set_overlay_radius(settings.overlay_radius_metres as f32);
    app.set_tab(if args.tab == SETTINGS_TAB { 1 } else { args.tab });
    if args.tab == 2 {
        app.set_dps_tab(2);
    }
    app.set_settings_open(args.tab == SETTINGS_TAB);
    app.set_meter_range(settings.meter_range as i32);
    app.set_pet_overlay(settings.pet_overlay);
    app.set_pet_overlay_target(settings.pet_overlay_target);
    app.set_pet_auto_pin(settings.pet_auto_pin);
    app.set_pet_species_names(ModelRc::new(VecModel::from(
        std::iter::once("All types".to_string()).chain(pet_species()).map(SharedString::from).collect::<Vec<_>>(),
    )));
    app.set_sidebar_width(settings.sidebar_width.clamp(240.0, 900.0));
    app.set_boss_pins_on(settings.boss_pins);
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
            let u = ui.borrow();
            u.selections.meter.set(next);
            u.selections.send(&u.commands);
            apply_meter(&app, &u);
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
        app.on_skill_mode_changed(move |_| apply_meter(&weak.unwrap(), &ui.borrow()));
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_chart_resized(move || {
            let app = weak.unwrap();
            if let Ok(u) = ui.try_borrow() {
                apply_timeline(&app, &u);
            }
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_chart_hover(move |at| {
            let app = weak.unwrap();
            app.set_chart_cursor(if at < 0.0 { -1.0 } else { at.clamp(0.0, 1.0) });
            apply_timeline(&app, &ui.borrow());
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_buff_mode_changed(move |_| apply_timeline(&weak.unwrap(), &ui.borrow()));
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_chart_click(move |x, y| {
            let hit = {
                let u = ui.borrow();
                let curves = u.chart_curves.borrow();
                chart::line_at(curves.0.iter().map(|(id, v)| (*id, v.as_slice())), curves.1, x, y)
            };
            if let Some(id) = hit {
                weak.unwrap().invoke_select_row(id);
            }
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_focus_boss(move |index| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let map = displayed_map(&u);
            if let Some(b) = bosses_of(map).get(index.max(0) as usize) {
                // a boss looked for is shown on the map, so it is found
                if u.settings.shown_bosses.insert(b.world_id) {
                    u.settings.save();
                }
                u.map.manual = Some(map);
                u.map.follow = false;
                u.map.center = (b.x as f64, b.y as f64);
                app.set_follow(false);
                apply_bosses(&app, &u);
                apply_map(&app, &mut u, true);
            }
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        let mapo = map_overlay.as_weak();
        app.on_toggle_boss(move |index, on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let Some(b) = bosses_of(displayed_map(&u)).get(index.max(0) as usize).copied() else { return };
            if on {
                u.settings.shown_bosses.insert(b.world_id);
            } else {
                u.settings.shown_bosses.remove(&b.world_id);
            }
            u.settings.save();
            apply_bosses(&app, &u);
            apply_map(&app, &mut u, true);
            apply_map_overlay(&mapo.unwrap(), &mut u, true);
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
            if let Ok(pet) = key.parse::<i32>() {
                toggle_pet(&mut u.settings, pet, on);
            }
            monsters_changed(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_toggle_monster(move |key, on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            if let Ok(npc) = key.parse::<i32>() {
                toggle_monster(&mut u.settings, npc, on);
            }
            monsters_changed(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_expand_pet(move |key| {
            let mut u = ui.borrow_mut();
            if let Ok(pet) = key.parse::<i32>() {
                if !u.filters.expanded_pets.remove(&pet) {
                    u.filters.expanded_pets.insert(pet);
                }
            }
            apply_filters(&weak.unwrap(), &u);
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
            let (groups, alone) = monster_groups(displayed_map(&u), u.filters.monster_query.trim());
            u.settings.pets.extend(groups.iter().map(|g| g.0));
            u.settings.monsters.extend(alone);
            monsters_changed(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_clear_monsters(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.settings.pets.clear();
            u.settings.monsters.clear();
            monsters_changed(&app, &mut u);
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
            } else if let Some(pet) = key.strip_prefix("p:").and_then(|id| id.parse::<i32>().ok()) {
                toggle_pet(&mut u.settings, pet, on);
            } else if let Some(npc) = key.strip_prefix("m:").and_then(|id| id.parse::<i32>().ok()) {
                toggle_monster(&mut u.settings, npc, on);
            }
            monsters_changed(&app, &mut u);
            apply_overlay_search(&o, &u);
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
            u.pet_overlay.set_locked(locked);
            if u.settings.pet_overlay {
                if let Err(e) = set_click_through(u.pet_overlay.window(), locked) {
                    errors.push(e);
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
            u.pet_overlay.set_overlay_opacity(v);
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
        let pan_ui = ui.clone();
        let mapo = map_overlay.as_weak();
        map_overlay.on_pan_by(move |dx, dy| {
            let o = mapo.unwrap();
            let ui = &pan_ui;
            let mut u = ui.borrow_mut();
            let (width, height) = (o.get_view_width() as f64, o.get_view_height() as f64);
            let ppu = View { center_x: 0.0, center_y: 0.0, radius_metres: u.settings.overlay_radius_metres, width, height }.pixels_per_unit();
            u.overlay_pan.0 -= dx as f64 / ppu;
            u.overlay_pan.1 -= dy as f64 / ppu;
            apply_map_overlay(&o, &mut u, true);
        });
        let ui = ui.clone();
        let mapo = map_overlay.as_weak();
        map_overlay.on_recenter(move || {
            let mut u = ui.borrow_mut();
            u.overlay_pan = (0.0, 0.0);
            apply_map_overlay(&mapo.unwrap(), &mut u, true);
        });
        let weak = map_overlay.as_weak();
        map_overlay.on_resize(move |edge| resize_window(&weak.unwrap(), resize_direction(edge)));
    }
    if let Some((w, h)) = settings.map_overlay_size {
        map_overlay.window().set_size(slint::LogicalSize::new(w, h));
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
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_pet_search(move |text| {
            let mut u = ui.borrow_mut();
            u.filters.pet_query = text.to_string();
            apply_pets(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_pet_filter_changed(move |_| apply_pets(&weak.unwrap(), &ui.borrow()));
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_pet_sort_changed(move || apply_pets(&weak.unwrap(), &ui.borrow()));
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_toggle_pin(move |pet, on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            set_pin(&mut u.settings, pet, on);
            monsters_changed(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_clear_pins(move || {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            for pet in u.settings.pinned_pets.clone() {
                set_pin(&mut u.settings, pet, false);
            }
            monsters_changed(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_pet_auto_pin_changed(move |on| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            u.settings.pet_auto_pin = on;
            start_pin_sync(&mut u.settings);
            monsters_changed(&app, &mut u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_boss_search(move |text| {
            let mut u = ui.borrow_mut();
            u.filters.boss_query = text.to_string();
            apply_bosses(&weak.unwrap(), &u);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        app.on_boss_pins_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.boss_pins = on;
            u.settings.save();
            apply_map(&weak.unwrap(), &mut u, false);
        });
    }
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        // a pinned card finds its boss: on its own map, if that is another one
        app.on_focus_pinned_boss(move |world_id| {
            let app = weak.unwrap();
            let mut u = ui.borrow_mut();
            let Some(b) = za2_core::catalog::field_bosses::all().iter().find(|b| b.world_id as i32 == world_id) else { return };
            let Some(map) = maps::all().iter().find(|m| m.table.ends_with(&format!("/{}", b.map_key))) else { return };
            u.map.manual = Some(map);
            u.map.follow = false;
            u.map.center = (b.x as f64, b.y as f64);
            app.set_follow(false);
            apply_bosses(&app, &u);
            apply_map(&app, &mut u, true);
        });
    }
    {
        let ui = ui.clone();
        app.on_sidebar_width_changed(move |width| {
            let mut u = ui.borrow_mut();
            u.settings.sidebar_width = width;
            u.settings.save();
        });
    }
    {
        let ui = ui.clone();
        app.on_pet_overlay_target_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.pet_overlay_target = on;
            u.settings.save();
            apply_pet_overlay(&u);
        });
    }
    {
        let ui = ui.clone();
        let overlay = pet_overlay.as_weak();
        app.on_pet_overlay_changed(move |on| {
            let mut u = ui.borrow_mut();
            u.settings.pet_overlay = on;
            u.settings.save();
            let o = overlay.unwrap();
            if on {
                apply_pet_overlay(&u);
                let _ = o.show();
                let locked = u.settings.overlays_locked;
                let weak = o.as_weak();
                slint::Timer::single_shot(Duration::from_millis(300), move || {
                    if let Some(o) = weak.upgrade() {
                        let _ = set_click_through(o.window(), locked);
                    }
                });
            } else {
                let _ = o.hide();
            }
        });
    }
    {
        let weak = pet_overlay.as_weak();
        pet_overlay.on_drag(move || move_window(&weak.unwrap()));
        let weak = pet_overlay.as_weak();
        pet_overlay.on_resize(move |edge| resize_window(&weak.unwrap(), resize_direction(edge)));
        let app_weak = app.as_weak();
        pet_overlay.on_toggle_lock(move || {
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
        app.on_window_resize(move |edge| resize_window(&weak.unwrap(), resize_direction(edge)));
    }

    let timer = slint::Timer::default();
    {
        let ui = ui.clone();
        let weak = app.as_weak();
        let mapo = map_overlay.as_weak();
        let mut filters_for: Option<String> = None;
        let mut learned_saved = false;
        let mut size_check = Instant::now();
        let mut timers_second = 0;
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
            if app.get_tab() == TIMERS_TAB {
                let second = wall_clock_ms() / 1000;
                if timers_second != second {
                    timers_second = second;
                    apply_timers(&app);
                }
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
                if app.get_tab() == PETS_TAB {
                    apply_pets(&app, &u);
                }
                if u.settings.pet_overlay {
                    apply_pet_overlay(&u);
                }
                if app.get_tab() == 1 && app.get_filter_tab() == 2 {
                    apply_bosses(&app, &u);
                }
                if app.get_tab() == PROFILE_TAB {
                    apply_profile(&app, &mut u);
                }
                if let Some(name) = u.snapshot.learned_name.clone() {
                    if !learned_saved && name != u.settings.own_character_name {
                        learned_saved = true;
                        u.settings.own_character_name = name.clone();
                        u.settings.save();
                        app.set_own_name(name.into());
                    }
                }
            }
            if app.get_tab() == PROFILE_TAB && u.profile_shown.is_none() {
                apply_profile(&app, &mut u);
            }
            if app.get_tab() == 1 && app.get_filter_tab() == 2 && app.get_boss_rows().row_count() == 0 && app.get_boss_summary().is_empty() {
                apply_bosses(&app, &u);
            }
            if app.get_tab() == PETS_TAB && app.get_pet_rows().row_count() == 0 && app.get_pet_filter() == 0 && u.filters.pet_query.is_empty() {
                apply_pets(&app, &u);
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
            if size_check.elapsed() >= Duration::from_secs(1) {
                size_check = Instant::now();
                remember_overlay_sizes(&mut u, &mapo);
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
                    "toggle-pet-overlay" => {
                        let on = !app.get_pet_overlay();
                        app.set_pet_overlay(on);
                        app.invoke_pet_overlay_changed(on);
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
    if settings.pet_overlay {
        app.invoke_pet_overlay_changed(true);
    }
    slint::run_event_loop_until_quit().ok();
    for m in &ui.borrow().overlays {
        let _ = m.window.hide();
    }
    let _ = map_overlay.hide();
    let _ = pet_overlay.hide();
    let u = ui.borrow();
    u.settings.save();
    control::cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_monster_is_found_by_its_name_under_its_pet() {
        let altgard = maps::by_key("altgard").unwrap();
        let (groups, _) = monster_groups(altgard, "poison blood gatherer");
        assert_eq!(groups, vec![(1195, vec![2400746])]);
    }

    #[test]
    fn unticking_a_monster_of_a_ticked_pet_keeps_the_others() {
        let mut settings = Settings::default();
        toggle_pet(&mut settings, 1195, true);
        let others: Vec<i32> = pets::npcs_of_pet(1195).iter().copied().filter(|&n| n != 2400746).collect();
        toggle_monster(&mut settings, 2400746, false);
        assert!(!settings.pets.contains(&1195));
        assert!(others.iter().all(|n| settings.monsters.contains(n)) && !settings.monsters.contains(&2400746));
    }

    #[test]
    fn pins_and_map_ticks_follow_each_other() {
        let mut settings = Settings::default();
        set_pin(&mut settings, 1195, true);
        sync_pins(&mut settings);
        assert!(settings.pets.contains(&1195) && settings.pinned_pets.contains(&1195), "pinning ticks the pet");
        toggle_pet(&mut settings, 1195, false);
        sync_pins(&mut settings);
        assert!(!settings.pinned_pets.contains(&1195), "unticking unpins");
        toggle_monster(&mut settings, 2400746, true);
        sync_pins(&mut settings);
        assert!(settings.pinned_pets.contains(&1195), "ticking a monster pins its pet");
        set_pin(&mut settings, 1195, false);
        sync_pins(&mut settings);
        assert!(!settings.monsters.contains(&2400746) && settings.pinned_pets.is_empty(), "unpinning unticks its monsters");
    }

    #[test]
    fn pins_made_before_the_sync_are_ticked() {
        let mut settings = Settings::default();
        settings.pinned_pets.insert(1195);
        start_pin_sync(&mut settings);
        assert!(settings.pets.contains(&1195) && settings.pinned_pets.contains(&1195));
    }

    #[test]
    fn gear_text_is_power_then_gear_score() {
        assert_eq!(gear_text(Some(113_456), Some(2410)), "113k · 2,410");
        assert_eq!(gear_text(Some(1_260_000), Some(12_345)), "1.3m · 12,345");
        assert_eq!(gear_text(None, Some(980)), "980");
        assert_eq!(gear_text(Some(0), None), "");
    }

    #[test]
    fn cube_sound_speeds_up_and_stops_at_the_cube() {
        assert_eq!(cube_beep_interval(f64::INFINITY), None, "no cube in view");
        assert_eq!(cube_beep_interval(2.0), None, "standing at the cube");
        let (near, far) = (cube_beep_interval(5.0).unwrap(), cube_beep_interval(40.0).unwrap());
        assert!(near < far && near >= 0.15 && far <= 2.0);
        assert_eq!(cube_beep_interval(200.0), Some(2.0));
    }
}
