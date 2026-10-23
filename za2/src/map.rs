use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};

use za2_core::catalog::gather::{self, GatherItem};
use za2_core::catalog::maps::MapInfo;
use za2_core::catalog::{bosses, map_icons, monsters, pets};
use za2_core::entities::{EnvObject, LiveMob};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub const TILE_DECODE_SIZE: u32 = 1024;
const TILE_MIN_SIZE: u32 = 128;
const TILE_CACHE_BYTES: usize = 192 << 20;
const TILE_THREADS: usize = 2;
const MAX_MARKERS: usize = 20_000;
const ANNOUNCE_RANGE: f32 = 12_000.0;
/// Range of the Hidden Cube radar circle around the player (world units).
pub const CUBE_RADAR_RANGE: f64 = 12_000.0;
/// A live object this much higher or lower than the player gets an arrow (5 m).
const LEVEL_DIFFERENCE: f32 = 500.0;

/// map, column, row
pub type TileCell = (String, u32, u32);
/// cell plus the decoded edge length in pixels
pub type TileKey = (TileCell, u32);

type TileRequest = (TileKey, std::path::PathBuf, std::path::PathBuf);

pub struct TileLoader {
    requests: Sender<TileRequest>,
    results: Receiver<(TileKey, Option<SharedPixelBuffer<Rgba8Pixel>>)>,
    cache: HashMap<TileKey, (Image, usize)>,
    order: VecDeque<TileKey>,
    bytes: usize,
    pending: HashSet<TileKey>,
    missing: HashSet<TileCell>,
}

fn decode_tile(color: &std::path::Path, mask: &std::path::Path, size: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let color = image::open(color).ok()?;
    let color = if color.width() > size { color.thumbnail(size, size) } else { color };
    let rgb = color.to_rgb8();
    let (w, h) = rgb.dimensions();
    // a tile without a mask is opaque (whole tiles, as the game's map shows them)
    let Some(alpha) = image::open(mask).ok().map(|m| m.to_luma8()) else {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        for (out, p) in buffer.make_mut_slice().iter_mut().zip(rgb.pixels()) {
            *out = Rgba8Pixel { r: p[0], g: p[1], b: p[2], a: 255 };
        }
        return Some(buffer);
    };
    let (mw, mh) = alpha.dimensions();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let pixels = buffer.make_mut_slice();
    for y in 0..h {
        let my = ((y as u64 * mh as u64) / h as u64).min(mh as u64 - 1) as u32;
        for x in 0..w {
            let mx = ((x as u64 * mw as u64) / w as u64).min(mw as u64 - 1) as u32;
            let p = rgb.get_pixel(x, y);
            pixels[(y * w + x) as usize] = Rgba8Pixel { r: p[0], g: p[1], b: p[2], a: alpha.get_pixel(mx, my)[0] };
        }
    }
    Some(buffer)
}

/// Decoded edge length for a tile drawn `on_screen` pixels wide: the next power of two, so zooming
/// does not decode a new copy for every wheel step.
pub fn decode_size_for(on_screen: f64) -> u32 {
    (on_screen.max(1.0).ceil() as u32).next_power_of_two().clamp(TILE_MIN_SIZE, TILE_DECODE_SIZE)
}

impl TileLoader {
    pub fn new() -> TileLoader {
        let (req_tx, req_rx) = channel::<TileRequest>();
        let (res_tx, res_rx) = channel();
        let req_rx = std::sync::Arc::new(std::sync::Mutex::new(req_rx));
        for i in 0..TILE_THREADS {
            let (req_rx, res_tx) = (req_rx.clone(), res_tx.clone());
            std::thread::Builder::new()
                .name(format!("za2-tiles-{i}"))
                .spawn(move || loop {
                    let next = req_rx.lock().map(|rx| rx.recv());
                    let Ok(Ok((key, color, mask))) = next else { return };
                    let decoded = if color.is_file() { decode_tile(&color, &mask, key.1) } else { None };
                    if res_tx.send((key, decoded)).is_err() {
                        return;
                    }
                })
                .expect("cannot start tile thread");
        }
        TileLoader {
            requests: req_tx,
            results: res_rx,
            cache: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            pending: HashSet::new(),
            missing: HashSet::new(),
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, buffer)) = self.results.try_recv() {
            self.pending.remove(&key);
            match buffer {
                Some(buffer) => {
                    let bytes = buffer.width() as usize * buffer.height() as usize * 4;
                    if let Some((_, old)) = self.cache.insert(key.clone(), (Image::from_rgba8(buffer), bytes)) {
                        self.bytes -= old;
                        self.order.retain(|k| *k != key);
                    }
                    self.bytes += bytes;
                    self.order.push_back(key);
                    while self.bytes > TILE_CACHE_BYTES && self.order.len() > 1 {
                        if let Some(old) = self.order.pop_front() {
                            if let Some((_, b)) = self.cache.remove(&old) {
                                self.bytes -= b;
                            }
                        }
                    }
                }
                None => {
                    self.missing.insert(key.0);
                }
            }
            any = true;
        }
        any
    }

    /// The tile at `size` pixels. While it is decoded, another cached size of the same tile is
    /// returned so the map does not blink while zooming.
    pub fn get(&mut self, map: &MapInfo, col: u32, row: u32, size: u32) -> Option<Image> {
        let cell = (map.key.clone(), col, row);
        if self.missing.contains(&cell) {
            return None;
        }
        let key = (cell, size);
        if let Some((image, _)) = self.cache.get(&key) {
            let image = image.clone();
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                let k = self.order.remove(pos).unwrap();
                self.order.push_back(k);
            }
            return Some(image);
        }
        if self.pending.insert(key.clone()) {
            let folder = map.tile_folder();
            let _ = self.requests.send((key.clone(), folder.join(format!("{col}_{row}.jpg")), folder.join(format!("{col}_{row}_a.png"))));
        }
        let cell = &key.0;
        std::iter::successors(Some(TILE_MIN_SIZE), |s| (*s < TILE_DECODE_SIZE).then_some(s * 2))
            .filter(|s| *s != size)
            .filter_map(|s| self.cache.get(&(cell.clone(), s)).map(|(image, _)| (s, image)))
            .min_by_key(|(s, _)| (s.abs_diff(size), std::cmp::Reverse(*s)))
            .map(|(_, image)| image.clone())
    }

    #[cfg(test)]
    pub fn loading(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    pub center_x: f64,
    pub center_y: f64,
    pub radius_metres: f64,
    pub width: f64,
    pub height: f64,
}

impl View {
    pub fn pixels_per_unit(&self) -> f64 {
        (self.width.min(self.height) / 2.0) / (self.radius_metres * 100.0)
    }

    pub fn to_screen(&self, map: &MapInfo, wx: f64, wy: f64) -> (f64, f64) {
        let k = self.pixels_per_unit() / map.scale;
        let (nx, ny) = map.to_native(self.center_x, self.center_y);
        let (px, py) = map.to_native(wx, wy);
        (self.width / 2.0 + (px - nx) * k, self.height / 2.0 + (py - ny) * k)
    }

    pub fn world_half_extent(&self) -> (f64, f64) {
        let ppu = self.pixels_per_unit();
        (self.width / 2.0 / ppu, self.height / 2.0 / ppu)
    }
}

pub struct TileQuad {
    pub image: Image,
    pub x: f64,
    pub y: f64,
    pub size: f64,
}

pub fn visible_tiles(map: &MapInfo, view: &View, loader: &mut TileLoader) -> Vec<TileQuad> {
    let k = view.pixels_per_unit() / map.scale;
    let (nx, ny) = map.to_native(view.center_x, view.center_y);
    let tile = map.tile as f64;
    let half_w = view.width / 2.0 / k;
    let half_h = view.height / 2.0 / k;
    let last = map.grid as i64 - 1;
    let c0 = (((nx - half_w) / tile).floor() as i64).max(0);
    let c1 = (((nx + half_w) / tile).floor() as i64).min(last);
    let r0 = (((ny - half_h) / tile).floor() as i64).max(0);
    let r1 = (((ny + half_h) / tile).floor() as i64).min(last);
    let size = decode_size_for(tile * k);
    let mut quads = Vec::new();
    for col in c0..=c1 {
        for row in r0..=r1 {
            if let Some(image) = loader.get(map, col as u32, row as u32, size) {
                quads.push(TileQuad {
                    image,
                    x: view.width / 2.0 + (col as f64 * tile - nx) * k,
                    y: view.height / 2.0 + (row as f64 * tile - ny) * k,
                    size: tile * k,
                });
            }
        }
    }
    quads
}

pub const PET_PALETTE: [(u8, u8, u8); 8] =
    [(0x4D, 0x9B, 0xFF), (0xFF, 0x8A, 0x3D), (0x5B, 0xD0, 0x7A), (0xE0, 0x5B, 0xD0), (0xFF, 0xE0, 0x4D), (0x4D, 0xDC, 0xE0), (0xFF, 0x5A, 0x6E), (0xB5, 0x8B, 0xFF)];

pub fn gather_color(kind: &str) -> (u8, u8, u8) {
    match kind {
        "Od" => (0x7F, 0xE3, 0xFF),
        "Herb" => (0x3D, 0xDC, 0x84),
        "Food" => (0xFF, 0xC2, 0x4D),
        "Ore" => (0xC9, 0xCE, 0xD6),
        "Wood" => (0xB5, 0x83, 0x5A),
        "Gemstone" => (0xE0, 0x8C, 0xFF),
        "Cotton" => (0xF3, 0xEB, 0xD3),
        "Fragment" => (0xFF, 0x7B, 0x7B),
        "Cube" => (0xFF, 0xD8, 0x4D),
        _ => (0xFF, 0xFF, 0xFF),
    }
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "Od" => "Odyle",
        "Herb" => "Herbs",
        "Food" => "Food",
        "Ore" => "Ore",
        "Wood" => "Wood",
        "Cotton" => "Cotton",
        "Gemstone" => "Gemstones",
        "Fragment" => "Empyrean Traces",
        "Cube" => "Hidden Cubes",
        _ => "Other",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    pub x: f32,
    pub y: f32,
    pub color: (u8, u8, u8),
    pub size: f32,
    pub live: bool,
    /// 1 above the player, -1 below, 0 about level or unknown.
    pub level: i32,
    /// Game icon (relative to the assets folder) drawn instead of the dot, in a ring of `color`.
    pub icon: Option<&'static str>,
    /// A monster without a portrait: the monster glyph in a ring of `color` instead of the dot.
    pub glyph: bool,
    /// The name shown under the marker (gathering spots, where one icon stands for several resources).
    pub name: Option<&'static str>,
}

/// How big a kind of marker is drawn: `base` pixels across at `MARKER_REF_RADIUS`, growing when zoomed in and shrinking
/// when zoomed out, but gently, and never outside `min`..`max`, so markers stay visible at every zoom without crowding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerSize {
    pub base: f32,
    pub min: f32,
    pub max: f32,
}

/// Live monsters picked in the monster list and live things with an icon.
pub const LIVE_ICON: MarkerSize = MarkerSize { base: 24.0, min: 20.0, max: 40.0 };
/// The other live monsters: smaller, so the picked ones stand out.
pub const LIVE_OTHER: MarkerSize = MarkerSize { base: 18.0, min: 14.0, max: 30.0 };
/// Live things without an icon.
pub const LIVE_DOT: MarkerSize = MarkerSize { base: 13.0, min: 10.0, max: 20.0 };
/// Field bosses picked in the boss list: bigger than the rest, so they are found at a glance.
pub const BOSS_ICON: MarkerSize = MarkerSize { base: 32.0, min: 28.0, max: 46.0 };
/// Spawn places and gathering spots with their icon.
pub const STATIC_ICON: MarkerSize = MarkerSize { base: 18.0, min: 13.0, max: 32.0 };
/// Spawn places and gathering spots as a dot: when zoomed far out (so hundreds of icons do not cover the map) or without icon.
pub const STATIC_DOT: MarkerSize = MarkerSize { base: 10.0, min: 8.0, max: 14.0 };
/// Static places are drawn as icons up to this zoom (radius in metres), as dots beyond.
const STATIC_ICON_RADIUS: f64 = 1500.0;
/// Markers have their base size at this zoom (radius in metres).
const MARKER_REF_RADIUS: f64 = 250.0;
/// How strongly the size follows the zoom: 0 not at all, 1 like the map itself.
const MARKER_ZOOM_FOLLOW: f64 = 0.3;

impl MarkerSize {
    /// The diameter in pixels at this zoom (radius in metres).
    pub fn at(self, radius_metres: f64) -> f32 {
        let factor = (MARKER_REF_RADIUS / radius_metres.max(1.0)).powf(MARKER_ZOOM_FOLLOW) as f32;
        (self.base * factor).clamp(self.min, self.max)
    }
}

/// Whether something at height `z` is clearly above (1) or below (-1) the player.
pub fn level_of(z: f32, player_z: Option<f32>) -> i32 {
    match player_z {
        Some(pz) if z.is_finite() && z - pz > LEVEL_DIFFERENCE => 1,
        Some(pz) if z.is_finite() && pz - z > LEVEL_DIFFERENCE => -1,
        _ => 0,
    }
}

/// A colour per Hidden Cube group: hues spread by the golden ratio, so neighbouring groups differ.
pub fn cube_group_color(index: usize) -> (u8, u8, u8) {
    let hue = (index as f64 * 0.618_033_988_75).fract() * 6.0;
    let (s, v) = (0.62, 1.0);
    let f = hue.fract();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match hue as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// A live cube is this close to one of its group's places.
const CUBE_GROUP_RANGE: f32 = 500.0;

#[derive(Default)]
pub struct StaticLayer {
    key: String,
    /// Gathering places with their marker colour (Hidden Cubes in the colour of their group), and whether the place is
    /// inside an area (which carries the name, so the marker does not).
    pub gather: Vec<(&'static GatherItem, f32, f32, (u8, u8, u8), bool)>,
    /// Every Hidden Cube place of the map with its group colour, for colouring live cubes.
    cube_places: Vec<(f32, f32, (u8, u8, u8))>,
    /// Spawn places of the selected monsters.
    pub monsters: Vec<(i32, f32, f32)>,
    /// Colour of each selected monster: one per ticked pet (its monsters share it), one per monster ticked alone.
    pub monster_colors: HashMap<i32, (u8, u8, u8)>,
    /// Where the selected monsters spawn, one outline per group of nearby spawn places.
    pub areas: Vec<Area>,
}

/// A group of selected monsters that share a colour: the monsters of a ticked pet, or one monster ticked on its own.
pub struct MonsterGroup {
    pub label: String,
    pub color: (u8, u8, u8),
    pub npcs: Vec<i32>,
}

/// The selected monsters of a map in groups: the monsters of each ticked pet in the pet's colour, then each monster
/// ticked on its own. Colours go round `PET_PALETTE` in that order, so the lists and the map agree.
pub fn monster_groups(table: &str, pet_ids: &BTreeSet<i32>, monster_ids: &BTreeSet<i32>) -> Vec<MonsterGroup> {
    let on_map: HashSet<i32> = monsters::on_map(table).into_iter().collect();
    let mut taken: HashSet<i32> = HashSet::new();
    let mut out: Vec<MonsterGroup> = Vec::new();
    for &pet in pet_ids {
        let npcs: Vec<i32> = pets::npcs_of_pet(pet).iter().copied().filter(|n| on_map.contains(n) && taken.insert(*n)).collect();
        if npcs.is_empty() {
            continue;
        }
        let label = pets::pet_name(pet, "en").map_or_else(|| format!("Pet #{pet}"), str::to_string);
        out.push(MonsterGroup { label, color: PET_PALETTE[out.len() % PET_PALETTE.len()], npcs });
    }
    for &npc in monster_ids {
        if on_map.contains(&npc) && taken.insert(npc) {
            let label = za2_core::catalog::npcs::name_of(npc, "en").map_or_else(|| format!("#{npc}"), str::to_string);
            out.push(MonsterGroup { label, color: PET_PALETTE[out.len() % PET_PALETTE.len()], npcs: vec![npc] });
        }
    }
    out
}

/// The selected monsters of a map with their colour (see `monster_groups`).
pub fn monster_colors(table: &str, pet_ids: &BTreeSet<i32>, monster_ids: &BTreeSet<i32>) -> Vec<(i32, (u8, u8, u8))> {
    monster_groups(table, pet_ids, monster_ids).into_iter().flat_map(|g| g.npcs.into_iter().map(move |n| (n, g.color))).collect()
}

/// Spawn places closer than this (50 m) belong to the same spawn area.
const AREA_LINK: f32 = 5000.0;
/// How far the border of a spawn area stays outside its spawn places (15 m).
const AREA_MARGIN: f32 = 1500.0;

/// Where a group of selected monsters spawns: the rounded outline around nearby spawn places (world units).
pub struct Area {
    pub label: String,
    pub color: (u8, u8, u8),
    pub outline: Vec<(f32, f32)>,
    /// The bounding box of the outline: min x, min y, max x, max y.
    pub bounds: (f32, f32, f32, f32),
}

/// Splits `points` into the groups of places linked by steps of at most `AREA_LINK`.
fn spawn_clusters(points: &[(f32, f32)]) -> Vec<Vec<(f32, f32)>> {
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut parent: Vec<usize> = (0..points.len()).collect();
    for i in 0..points.len() {
        for j in i + 1..points.len() {
            let (dx, dy) = (points[i].0 - points[j].0, points[i].1 - points[j].1);
            if dx.abs() <= AREA_LINK && dy.abs() <= AREA_LINK && dx * dx + dy * dy <= AREA_LINK * AREA_LINK {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<(f32, f32)>> = HashMap::new();
    for (i, &p) in points.iter().enumerate() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(p);
    }
    let mut out: Vec<Vec<(f32, f32)>> = groups.into_values().collect();
    out.sort_by(|a, b| a[0].0.total_cmp(&b[0].0).then(a[0].1.total_cmp(&b[0].1)));
    out
}

/// The convex hull of `points` (counter-clockwise, monotone chain).
fn convex_hull(mut points: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut hull: Vec<(f32, f32)> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &(f32, f32)>> = if pass == 0 { Box::new(points.iter()) } else { Box::new(points.iter().rev()) };
        for &p in iter {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// The outline `AREA_MARGIN` around a group of places: the hull of a circle around each place.
fn rounded_outline(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    const STEPS: usize = 20;
    let ring: Vec<(f32, f32)> = points
        .iter()
        .flat_map(|&(x, y)| {
            (0..STEPS).map(move |k| {
                let a = k as f32 * std::f32::consts::TAU / STEPS as f32;
                (x + AREA_MARGIN * a.cos(), y + AREA_MARGIN * a.sin())
            })
        })
        .collect();
    convex_hull(ring)
}

/// Gathering places of one resource this many or more close together are drawn as an area.
const GATHER_AREA_MIN: usize = 3;

/// The area around one group of places.
fn area(label: &str, color: (u8, u8, u8), cluster: &[(f32, f32)]) -> Area {
    let outline = rounded_outline(cluster);
    let bounds = outline.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, &(x, y)| (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y)));
    Area { label: label.to_string(), color, outline, bounds }
}

pub fn spawn_areas(label: &str, color: (u8, u8, u8), points: &[(f32, f32)]) -> Vec<Area> {
    spawn_clusters(points).into_iter().map(|cluster| area(label, color, &cluster)).collect()
}

/// The icon of a monster: its portrait, else the portrait of its pet (none: drawn as the monster glyph).
pub fn monster_icon(npc: i32) -> Option<&'static str> {
    monsters::info(npc).and_then(|m| m.icon.as_deref()).or_else(|| pets::pets_of_npc(npc).iter().find_map(|&p| pets::icon_of(p)))
}

/// The ring colour of a monster that is not selected, by its rank (normal, elite, boss).
pub fn rank_color(npc: i32) -> (u8, u8, u8) {
    if monsters::is_boss_rank(npc) || bosses::find(npc).is_some() {
        (0xFF, 0xC8, 0x3D)
    } else if monsters::info(npc).is_some_and(|m| m.rank == "Elite") {
        (0xFF, 0x8A, 0x3D)
    } else {
        (0xE0, 0x50, 0x50)
    }
}

impl StaticLayer {
    /// The group colour of the Hidden Cube place nearest to (x, y), if one is close.
    pub fn cube_color_near(&self, x: f32, y: f32) -> Option<(u8, u8, u8)> {
        self.cube_places
            .iter()
            .map(|&(px, py, color)| ((px - x).powi(2) + (py - y).powi(2), color))
            .filter(|(d2, _)| *d2 <= CUBE_GROUP_RANGE * CUBE_GROUP_RANGE)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, color)| color)
    }

    pub fn update(&mut self, map: Option<&MapInfo>, gather_keys: &BTreeSet<String>, pet_ids: &BTreeSet<i32>, monster_ids: &BTreeSet<i32>, faction: Option<&str>) {
        let key = format!("{}|{:?}|{:?}|{:?}|{faction:?}", map.map_or("-", |m| m.key.as_str()), gather_keys, pet_ids, monster_ids);
        if key == self.key {
            return;
        }
        self.key = key;
        self.gather.clear();
        self.cube_places.clear();
        self.monsters.clear();
        self.monster_colors.clear();
        self.areas.clear();
        let Some(map) = map else { return };
        for (i, group) in gather::cube_groups(&map.table).iter().enumerate() {
            let color = cube_group_color(i);
            self.cube_places.extend(group.iter().map(|&(x, y)| (x, y, color)));
        }
        for item in gather::data().items.iter().filter(|i| gather_keys.contains(&i.key) && i.is_for(faction)) {
            if item.kind == "Cube" && !self.cube_places.is_empty() {
                self.gather.extend(self.cube_places.iter().map(|&(x, y, color)| (item, x, y, color, false)));
                continue;
            }
            let color = gather_color(&item.kind);
            let points = gather::points_of(&map.table, &item.key);
            // Odyle and Hidden Cubes have an icon of their own; the other resources share one per kind, so places close
            // together become an area with the resource's name, like the monsters' spawn areas
            if matches!(item.kind.as_str(), "Od" | "Cube") {
                self.gather.extend(points.iter().map(|&(x, y)| (item, x, y, color, false)));
                continue;
            }
            for cluster in spawn_clusters(points) {
                let in_area = cluster.len() >= GATHER_AREA_MIN;
                if in_area {
                    self.areas.push(area(item.name_in("en"), color, &cluster));
                }
                self.gather.extend(cluster.into_iter().map(|(x, y)| (item, x, y, color, in_area)));
            }
        }
        for group in monster_groups(&map.table, pet_ids, monster_ids) {
            let mut places: Vec<(f32, f32)> = Vec::new();
            for &npc in &group.npcs {
                self.monster_colors.insert(npc, group.color);
                let spawns = monsters::spawns_of(&map.table, npc);
                self.monsters.extend(spawns.iter().map(|&(x, y)| (npc, x, y)));
                places.extend(spawns.iter().copied());
            }
            self.areas.extend(spawn_areas(&group.label, group.color, &places));
        }
    }
}

/// A spawn area on the screen (logical pixels), with the place of its name: the middle under its bottom edge.
pub struct ScreenArea {
    pub outline: Vec<(f32, f32)>,
    pub color: (u8, u8, u8),
    pub label: String,
    pub label_at: (f32, f32),
}

/// The spawn areas of `layer` that reach into the view, in screen coordinates.
pub fn screen_areas(map: &MapInfo, view: &View, layer: &StaticLayer) -> Vec<ScreenArea> {
    let (hx, hy) = view.world_half_extent();
    layer
        .areas
        .iter()
        .filter(|a| {
            let (x0, y0, x1, y1) = a.bounds;
            x1 as f64 >= view.center_x - hx && x0 as f64 <= view.center_x + hx && y1 as f64 >= view.center_y - hy && y0 as f64 <= view.center_y + hy
        })
        .map(|a| {
            let outline: Vec<(f32, f32)> = a
                .outline
                .iter()
                .map(|&(x, y)| {
                    let (sx, sy) = view.to_screen(map, x as f64, y as f64);
                    (sx as f32, sy as f32)
                })
                .collect();
            let (min_x, max_x, max_y) = outline.iter().fold((f32::MAX, f32::MIN, f32::MIN), |b, &(x, y)| (b.0.min(x), b.1.max(x), b.2.max(y)));
            ScreenArea { label: a.label.clone(), color: a.color, label_at: ((min_x + max_x) / 2.0, max_y), outline }
        })
        .collect()
}

/// Draws the markers and spawn areas of a view into one image. Hundreds of markers as UI elements (each a clipped icon in
/// a ring, a glyph made of paths) made the map slow; one image is cheap to show, and each kind of marker is drawn once
/// and then copied.
#[derive(Default)]
pub struct LayerPainter {
    sprites: HashMap<SpriteKey, tiny_skia::Pixmap>,
    icons: HashMap<&'static str, Option<tiny_skia::Pixmap>>,
    canvas: Option<tiny_skia::Pixmap>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SpriteKey {
    icon: Option<&'static str>,
    glyph: bool,
    color: (u8, u8, u8),
    live: bool,
    level: i32,
    /// diameter in physical pixels, times 4
    size: u32,
}

/// Room around a sprite for the level arrow, in diameters.
const SPRITE_PAD: f32 = 0.4;

fn paint_color((r, g, b): (u8, u8, u8), alpha: f32) -> tiny_skia::Paint<'static> {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
    paint.anti_alias = true;
    paint
}

fn circle(cx: f32, cy: f32, r: f32) -> Option<tiny_skia::Path> {
    tiny_skia::PathBuilder::from_circle(cx, cy, r.max(0.1))
}

/// The monster glyph (a skull) in a 24 x 24 box.
fn skull_path() -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    pb.push_circle(12.0, 10.5, 7.5);
    pb.move_to(8.5, 16.5);
    pb.line_to(8.5, 21.0);
    pb.line_to(15.5, 21.0);
    pb.line_to(15.5, 16.5);
    pb.move_to(12.0, 18.0);
    pb.line_to(12.0, 21.0);
    pb.finish()
}

fn load_icon(relative: &str) -> Option<tiny_skia::Pixmap> {
    let rgba = image::open(za2_core::assets::path(relative)).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut data = rgba.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        for c in &mut px[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
    }
    tiny_skia::Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)
}

impl LayerPainter {
    fn sprite(&mut self, key: SpriteKey) -> Option<&tiny_skia::Pixmap> {
        if !self.sprites.contains_key(&key) {
            if self.sprites.len() > 2048 {
                self.sprites.clear();
            }
            let icon = key.icon.and_then(|i| self.icons.entry(i).or_insert_with(|| load_icon(i)).clone());
            let sprite = draw_sprite(key, icon.as_ref())?;
            self.sprites.insert(key, sprite);
        }
        self.sprites.get(&key)
    }

    /// The markers and areas of a `width` x `height` view (logical pixels) at `scale` physical pixels per logical one.
    pub fn paint(&mut self, width: f64, height: f64, scale: f32, markers: &[Marker], areas: &[ScreenArea]) -> Image {
        let (w, h) = (((width as f32 * scale).ceil() as u32).max(1), ((height as f32 * scale).ceil() as u32).max(1));
        let mut canvas = match self.canvas.take() {
            Some(mut c) if c.width() == w && c.height() == h => {
                c.fill(tiny_skia::Color::TRANSPARENT);
                c
            }
            _ => match tiny_skia::Pixmap::new(w, h) {
                Some(c) => c,
                None => return Image::default(),
            },
        };
        let to_physical = tiny_skia::Transform::from_scale(scale, scale);
        for area in areas {
            let mut pb = tiny_skia::PathBuilder::new();
            for (i, &(x, y)) in area.outline.iter().enumerate() {
                if i == 0 {
                    pb.move_to(x, y);
                } else {
                    pb.line_to(x, y);
                }
            }
            pb.close();
            if let Some(path) = pb.finish() {
                canvas.fill_path(&path, &paint_color(area.color, 0.14), tiny_skia::FillRule::Winding, to_physical, None);
                let stroke = tiny_skia::Stroke { width: 2.0, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
                canvas.stroke_path(&path, &paint_color(area.color, 0.9), &stroke, to_physical, None);
            }
        }
        for m in markers {
            let d = m.size * scale;
            let key = SpriteKey { icon: m.icon, glyph: m.glyph, color: m.color, live: m.live, level: m.level, size: (d * 4.0).round() as u32 };
            let Some(sprite) = self.sprite(key) else { continue };
            let pad = (sprite.width() as f32 - d) / 2.0;
            let (x, y) = ((m.x * scale - d / 2.0 - pad).round() as i32, (m.y * scale - d / 2.0 - pad).round() as i32);
            canvas.draw_pixmap(x, y, sprite.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
        }
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(canvas.data(), w, h);
        self.canvas = Some(canvas);
        Image::from_rgba8_premultiplied(buffer)
    }
}

/// One marker as `MapView` drew it: a dot, or the icon (or the monster glyph) in a ring of the marker colour, with an arrow
/// when it is above or below the player.
fn draw_sprite(key: SpriteKey, icon: Option<&tiny_skia::Pixmap>) -> Option<tiny_skia::Pixmap> {
    let d = key.size as f32 / 4.0;
    let pad = (d * SPRITE_PAD).ceil().max(7.0);
    let side = (d + 2.0 * pad).ceil() as u32;
    let mut pm = tiny_skia::Pixmap::new(side, side)?;
    let (c, r) = (pad + d / 2.0, d / 2.0);
    let fill = tiny_skia::FillRule::Winding;
    let id = tiny_skia::Transform::identity();
    let opacity = if key.live { 1.0 } else { 0.8 };
    let ringed = icon.is_some() || key.glyph;
    if ringed {
        let ring = (d * 0.09).max(2.0);
        pm.fill_path(&circle(c, c, r)?, &paint_color((0, 0, 0), 0xa0 as f32 / 255.0), fill, id, None);
        if let Some(icon) = icon {
            let inner = r - ring;
            let k = (2.0 * inner) / icon.width().max(icon.height()) as f32;
            let (ox, oy) = (c - icon.width() as f32 * k / 2.0, c - icon.height() as f32 * k / 2.0);
            let shader = tiny_skia::Pattern::new(
                icon.as_ref(),
                tiny_skia::SpreadMode::Pad,
                tiny_skia::FilterQuality::Bicubic,
                opacity,
                tiny_skia::Transform::from_scale(k, k).post_translate(ox, oy),
            );
            let paint = tiny_skia::Paint { shader, anti_alias: true, ..Default::default() };
            pm.fill_path(&circle(c, c, inner)?, &paint, fill, id, None);
        } else {
            let box_side = d * 0.6;
            let k = box_side / 24.0;
            let stroke = tiny_skia::Stroke { width: (d * 0.07).max(1.2) / k, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
            let at = tiny_skia::Transform::from_scale(k, k).post_translate(c - box_side / 2.0, c - box_side / 2.0);
            let skull = skull_path()?;
            pm.stroke_path(&skull, &paint_color(key.color, opacity), &stroke, at, None);
            for ex in [9.0, 15.0] {
                pm.fill_path(&circle(ex, 11.0, 1.4)?, &paint_color(key.color, opacity), fill, at, None);
            }
        }
        let stroke = tiny_skia::Stroke { width: ring, ..Default::default() };
        pm.stroke_path(&circle(c, c, r - ring / 2.0)?, &paint_color(key.color, if key.live { 1.0 } else { 0.6 }), &stroke, id, None);
    } else {
        pm.fill_path(&circle(c, c, r)?, &paint_color(key.color, 1.0), fill, id, None);
        let (border, color, alpha) = if key.live { (2.0, (255, 255, 255), 1.0) } else { (1.0, (0, 0, 0), 0x99 as f32 / 255.0) };
        let stroke = tiny_skia::Stroke { width: border, ..Default::default() };
        pm.stroke_path(&circle(c, c, r - border / 2.0)?, &paint_color(color, alpha), &stroke, id, None);
    }
    if key.level != 0 {
        // a triangle at the top right, pointing up (above the player) or down (below)
        let k = (d / 22.0).max(1.0);
        let (x0, y0) = (pad + d - 3.0 * k, pad - 6.0 * k);
        let mut pb = tiny_skia::PathBuilder::new();
        if key.level > 0 {
            pb.move_to(x0 + 4.5 * k, y0);
            pb.line_to(x0 + 9.0 * k, y0 + 8.0 * k);
            pb.line_to(x0, y0 + 8.0 * k);
        } else {
            pb.move_to(x0, y0);
            pb.line_to(x0 + 9.0 * k, y0);
            pb.line_to(x0 + 4.5 * k, y0 + 8.0 * k);
        }
        pb.close();
        let arrow = pb.finish()?;
        pm.fill_path(&arrow, &paint_color((255, 255, 255), 1.0), fill, id, None);
        pm.stroke_path(&arrow, &paint_color((0, 0, 0), 1.0), &tiny_skia::Stroke { width: 1.0, ..Default::default() }, id, None);
    }
    Some(pm)
}

/// A field boss at world (x, y) as a marker on the screen: its portrait (or the monster glyph) in a ring of `color`.
/// None when it is out of view.
pub fn boss_marker(map: &MapInfo, view: &View, npc: i32, x: f32, y: f32, color: (u8, u8, u8)) -> Option<Marker> {
    let (hx, hy) = view.world_half_extent();
    if (x as f64 - view.center_x).abs() > hx * 1.05 || (y as f64 - view.center_y).abs() > hy * 1.05 {
        return None;
    }
    let (sx, sy) = view.to_screen(map, x as f64, y as f64);
    let icon = monster_icon(npc);
    Some(Marker { x: sx as f32, y: sy as f32, color, size: BOSS_ICON.at(view.radius_metres), live: true, level: 0, icon, glyph: icon.is_none(), name: None })
}

pub struct LiveInputs<'a> {
    pub known: bool,
    pub player: (f32, f32),
    pub player_z: Option<f32>,
    pub env: &'a [EnvObject],
    pub mobs: &'a [LiveMob],
    pub show_live_mobs: bool,
    pub show_cubes: bool,
}

pub fn markers(map: &MapInfo, view: &View, layer: &StaticLayer, live: &LiveInputs, out: &mut Vec<Marker>) {
    out.clear();
    let (hx, hy) = view.world_half_extent();
    let margin = 1.05;
    let in_view = |x: f32, y: f32| ((x as f64 - view.center_x).abs() <= hx * margin) && ((y as f64 - view.center_y).abs() <= hy * margin);
    let near_player = |x: f32, y: f32| {
        let (dx, dy) = (x - live.player.0, y - live.player.1);
        live.known && dx.abs() < ANNOUNCE_RANGE && dy.abs() < ANNOUNCE_RANGE && dx * dx + dy * dy < ANNOUNCE_RANGE * ANNOUNCE_RANGE
    };
    let live_env_matches = |key: &str, x: f32, y: f32| live.env.iter().any(|e| e.key == key && (e.x - x).abs() < 300.0 && (e.y - y).abs() < 300.0);
    let radius = view.radius_metres;
    let static_icons = radius <= STATIC_ICON_RADIUS;
    let static_dot = STATIC_DOT.at(radius);
    // `glyph`: a monster, drawn as the monster glyph when it has no icon
    let static_marker = |x: f32, y: f32, color, icon: Option<&'static str>, glyph: bool| match (icon, glyph) {
        _ if !static_icons => Marker { x, y, color, size: static_dot, live: false, level: 0, icon: None, glyph: false, name: None },
        (None, false) => Marker { x, y, color, size: static_dot, live: false, level: 0, icon: None, glyph: false, name: None },
        _ => Marker { x, y, color, size: STATIC_ICON.at(radius), live: false, level: 0, icon, glyph: icon.is_none(), name: None },
    };
    let live_marker = |x: f32, y: f32, color, level: i32, icon: Option<&'static str>, glyph: bool| Marker {
        x,
        y,
        color,
        size: if icon.is_some() || glyph { LIVE_ICON } else { LIVE_DOT }.at(radius),
        live: true,
        level,
        icon,
        glyph: glyph && icon.is_none(),
        name: None,
    };
    // the name of a gathering spot, while the icons are shown; Odyle and Hidden Cubes have an icon of their own
    let name_of = |item: Option<&'static GatherItem>| item.filter(|i| static_icons && !matches!(i.kind.as_str(), "Od" | "Cube")).map(|i| i.name_in("en"));

    let push = |m: Marker, out: &mut Vec<Marker>| {
        if out.len() < MAX_MARKERS {
            let (sx, sy) = view.to_screen(map, m.x as f64, m.y as f64);
            out.push(Marker { x: sx as f32, y: sy as f32, ..m });
        }
    };

    for &(npc, x, y) in &layer.monsters {
        if in_view(x, y) {
            let color = layer.monster_colors.get(&npc).copied().unwrap_or((255, 255, 255));
            push(static_marker(x, y, color, monster_icon(npc), true), out);
        }
    }

    for &(item, x, y, color, in_area) in &layer.gather {
        if !in_view(x, y) {
            continue;
        }
        if near_player(x, y) && !live_env_matches(&item.key, x, y) {
            continue;
        }
        let name = if in_area { None } else { name_of(Some(item)) };
        push(Marker { name, ..static_marker(x, y, color, map_icons::gather(&item.kind), false) }, out);
    }

    if live.known {
        for e in live.env {
            let wanted = if e.kind == "Cube" { live.show_cubes } else { layer.gather.iter().any(|g| g.0.key == e.key) };
            if wanted && in_view(e.x, e.y) {
                let color = if e.kind == "Cube" { layer.cube_color_near(e.x, e.y).unwrap_or_else(|| gather_color("Cube")) } else { gather_color(&e.kind) };
                let marker = live_marker(e.x, e.y, color, level_of(e.z, live.player_z), map_icons::gather(&e.kind), false);
                push(Marker { name: name_of(gather::item(&e.key)), ..marker }, out);
            }
        }
        for &LiveMob { npc, x, y, .. } in live.mobs {
            if !in_view(x, y) {
                continue;
            }
            match layer.monster_colors.get(&npc) {
                Some(&color) => push(live_marker(x, y, color, 0, monster_icon(npc), true), out),
                None if live.show_live_mobs => {
                    // the other monsters are drawn smaller in a ring of their rank colour, so the selected ones stand out
                    let marker = live_marker(x, y, rank_color(npc), 0, monster_icon(npc), true);
                    push(Marker { size: LIVE_OTHER.at(radius), ..marker }, out);
                }
                None => {}
            }
        }
    }
}

/// Most marker names shown at once: each one is a UI element.
const MAX_NAMES: usize = 200;

/// A name under a marker, centred on (x, y) (screen, logical pixels).
pub struct MarkerName {
    pub x: f32,
    pub y: f32,
    pub text: &'static str,
    pub color: (u8, u8, u8),
}

/// The names of the markers that carry one, live ones first; a name that would cover one already placed is left out.
pub fn marker_names(markers: &[Marker]) -> Vec<MarkerName> {
    let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut out = Vec::new();
    for m in markers.iter().filter(|m| m.live).chain(markers.iter().filter(|m| !m.live)) {
        let Some(text) = m.name else { continue };
        let y = m.y + m.size / 2.0;
        // about the size of the chip the UI draws (11px bold text, 5px padding, 18px high, 2px below)
        let half = text.chars().count() as f32 * 3.3 + 5.0;
        let b = (m.x - half, y, m.x + half, y + 22.0);
        if boxes.iter().any(|o| b.0 < o.2 && o.0 < b.2 && b.1 < o.3 && o.1 < b.3) {
            continue;
        }
        boxes.push(b);
        out.push(MarkerName { x: m.x, y, text, color: m.color });
        if out.len() >= MAX_NAMES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use za2_core::catalog::maps;

    #[test]
    fn view_centre_maps_to_view_middle() {
        let map = maps::by_key("verteron").expect("maps.json");
        let view = View { center_x: 1000.0, center_y: -2000.0, radius_metres: 100.0, width: 400.0, height: 300.0 };
        let (x, y) = view.to_screen(map, 1000.0, -2000.0);
        assert!((x - 200.0).abs() < 1e-6 && (y - 150.0).abs() < 1e-6);
        let (x2, _) = view.to_screen(map, 1000.0 + 10_000.0, -2000.0);
        assert!((x2 - 200.0 - 150.0).abs() < 1e-6);
    }

    #[test]
    fn tile_quads_line_up_with_native_space() {
        let map = maps::by_key("verteron").expect("maps.json");
        let (wx, wy) = map.to_world(map.tile as f64, map.tile as f64);
        let view = View { center_x: wx, center_y: wy, radius_metres: 50.0, width: 200.0, height: 200.0 };
        let (x, y) = view.to_screen(map, wx, wy);
        assert!((x - 100.0).abs() < 1e-6 && (y - 100.0).abs() < 1e-6);
    }

    #[test]
    fn cube_groups_get_distinct_colours() {
        let colours: std::collections::HashSet<_> = (0..59).map(cube_group_color).collect();
        assert!(colours.len() >= 55, "neighbouring group indices must not share a colour");
        assert_ne!(cube_group_color(0), cube_group_color(1));
    }

    #[test]
    fn altgard_has_its_cube_groups_and_full_resources() {
        let map = altgard();
        assert_eq!(gather::cube_groups(&map.table).len(), 59);
        let ruby = gather::data().items.iter().find(|i| i.key == "Ruby" && gather::points_of(&map.table, &i.key).len() > 100);
        assert!(ruby.is_some(), "the full extraction has well over 100 Rubies in Altgard");
    }

    fn altgard() -> &'static MapInfo {
        maps::by_key("altgard").expect("maps.json")
    }

    #[test]
    fn close_gathering_places_become_named_areas() {
        let mut layer = StaticLayer::default();
        layer.update(Some(altgard()), &BTreeSet::from(["Ruby".to_string()]), &BTreeSet::new(), &BTreeSet::new(), None);
        assert!(!layer.areas.is_empty() && layer.areas.iter().all(|a| a.label == "Ruby"));
        assert!(layer.gather.iter().filter(|g| g.4).count() >= GATHER_AREA_MIN, "the places in an area lose their own name");
    }

    #[test]
    fn level_arrows_need_both_heights() {
        assert_eq!(level_of(27_000.0, Some(26_000.0)), 1);
        assert_eq!(level_of(25_000.0, Some(26_000.0)), -1);
        assert_eq!(level_of(26_400.0, Some(26_000.0)), 0);
        assert_eq!(level_of(f32::NAN, Some(26_000.0)), 0);
        assert_eq!(level_of(27_000.0, None), 0);
    }

    #[test]
    fn markers_follow_the_zoom_within_bounds() {
        for kind in [LIVE_ICON, LIVE_OTHER, LIVE_DOT, STATIC_ICON, STATIC_DOT] {
            assert_eq!(kind.at(MARKER_REF_RADIUS), kind.base);
            assert!(kind.at(50.0) > kind.at(100.0) && kind.at(100.0) > kind.base);
            assert!(kind.at(1000.0) < kind.base);
            // the map zooms from 25 m to 8 km: never smaller than the minimum, never bigger than the maximum
            assert_eq!(kind.at(8000.0), kind.min);
            assert!(kind.at(25.0) <= kind.max);
        }
        // picked monsters stay bigger than the others at every zoom
        for radius in [25.0, 100.0, 250.0, 1000.0, 8000.0] {
            assert!(LIVE_ICON.at(radius) > LIVE_OTHER.at(radius));
        }
    }

    #[test]
    fn spawn_areas_split_far_groups_and_hold_every_place() {
        let places = [(0.0, 0.0), (3000.0, 0.0), (3000.0, 2500.0), (100_000.0, 0.0)];
        let areas = spawn_areas("Wolf", (255, 0, 0), &places);
        assert_eq!(areas.len(), 2, "places 50 m apart share an area, 1 km away is another");
        for &(x, y) in &places {
            let area = areas.iter().find(|a| x >= a.bounds.0 && x <= a.bounds.2).unwrap();
            assert!(x - area.bounds.0 >= AREA_MARGIN * 0.95 && area.bounds.2 - x >= AREA_MARGIN * 0.95);
            assert!(y - area.bounds.1 >= AREA_MARGIN * 0.95 && area.bounds.3 - y >= AREA_MARGIN * 0.95);
        }
        assert!(areas.iter().all(|a| a.outline.len() >= 8 && a.label == "Wolf"));
    }

    #[test]
    fn the_layer_shows_markers_and_areas() {
        let mut painter = LayerPainter::default();
        let markers = [
            Marker { x: 20.0, y: 20.0, color: (255, 0, 0), size: 20.0, live: true, level: 1, icon: None, glyph: true, name: None },
            Marker { x: 60.0, y: 20.0, color: (0, 255, 0), size: 10.0, live: false, level: 0, icon: None, glyph: false, name: None },
        ];
        let areas = [ScreenArea { outline: vec![(10.0, 50.0), (90.0, 50.0), (50.0, 90.0)], color: (0, 0, 255), label: "x".into(), label_at: (50.0, 90.0) }];
        let image = painter.paint(100.0, 100.0, 2.0, &markers, &areas);
        assert_eq!((image.size().width, image.size().height), (200, 200));
        let pixels = image.to_rgba8_premultiplied().unwrap();
        let alpha = |x: usize, y: usize| pixels.as_slice()[y * 200 + x].a;
        assert!(alpha(120, 40) > 200, "the green dot");
        assert!(alpha(100, 140) > 0, "inside the area");
        assert_eq!(alpha(190, 190), 0, "nothing outside");
        // the same markers again come from the sprite cache
        let sprites = painter.sprites.len();
        painter.paint(100.0, 100.0, 2.0, &markers, &areas);
        assert_eq!(painter.sprites.len(), sprites);
    }

    #[test]
    fn decode_size_follows_zoom() {
        assert_eq!(decode_size_for(75.0), TILE_MIN_SIZE);
        assert_eq!(decode_size_for(300.0), 512);
        assert_eq!(decode_size_for(5000.0), TILE_DECODE_SIZE);
    }

    #[test]
    fn whole_map_view_settles() {
        let map = maps::by_key("verteron").expect("maps.json");
        let half = map.native_size() / 2.0;
        let (cx, cy) = map.to_world(half, half);
        let view = View { center_x: cx, center_y: cy, radius_metres: half / map.scale / 100.0, width: 700.0, height: 580.0 };
        let mut loader = TileLoader::new();
        let mut shown = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            shown = visible_tiles(map, &view, &mut loader).len();
            if !loader.loading() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            loader.poll();
        }
        assert!(!loader.loading());
        let on_disk = std::fs::read_dir(map.tile_folder()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".jpg")).count();
        assert_eq!(shown, on_disk);
        visible_tiles(map, &view, &mut loader);
        assert!(!loader.loading(), "a settled view must not request tiles again");
        assert!(loader.bytes <= TILE_CACHE_BYTES);
    }
}
