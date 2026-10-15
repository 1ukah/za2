use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};

use za2_core::catalog::gather::{self, GatherItem};
use za2_core::catalog::maps::MapInfo;
use za2_core::catalog::pets;
use za2_core::entities::{EnvObject, LiveMob};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub const TILE_DECODE_SIZE: u32 = 1024;
const TILE_MIN_SIZE: u32 = 128;
const TILE_CACHE_BYTES: usize = 192 << 20;
const TILE_THREADS: usize = 2;
const MAX_MARKERS: usize = 4000;
const ANNOUNCE_RANGE: f32 = 4000.0;
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
    let alpha = image::open(mask).ok()?.to_luma8();
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
                    let decoded = if color.is_file() && mask.is_file() { decode_tile(&color, &mask, key.1) } else { None };
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
        "Od" => "Odyle (Od)",
        "Herb" => "Herbs and plants",
        "Food" => "Food",
        "Ore" => "Ore",
        "Wood" => "Wood",
        "Cotton" => "Cotton",
        "Gemstone" => "Gemstones",
        "Fragment" => "Empyrean Traces",
        "Cube" => "Hidden Cubes (static places)",
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
    /// Gathering places with their marker colour (Hidden Cubes in the colour of their group).
    pub gather: Vec<(&'static GatherItem, f32, f32, (u8, u8, u8))>,
    /// Every Hidden Cube place of the map with its group colour, for colouring live cubes.
    cube_places: Vec<(f32, f32, (u8, u8, u8))>,
    pub pets: Vec<(i32, f32, f32)>,
    pub pet_colors: HashMap<i32, (u8, u8, u8)>,
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

    pub fn update(&mut self, map: Option<&MapInfo>, gather_keys: &BTreeSet<String>, pet_ids: &BTreeSet<i32>, faction: Option<&str>) {
        let key = format!("{}|{:?}|{:?}|{faction:?}", map.map_or("-", |m| m.key.as_str()), gather_keys, pet_ids);
        if key == self.key {
            return;
        }
        self.key = key;
        self.gather.clear();
        self.cube_places.clear();
        self.pets.clear();
        self.pet_colors.clear();
        let Some(map) = map else { return };
        for (i, group) in gather::cube_groups(&map.table).iter().enumerate() {
            let color = cube_group_color(i);
            self.cube_places.extend(group.iter().map(|&(x, y)| (x, y, color)));
        }
        for item in gather::data().items.iter().filter(|i| gather_keys.contains(&i.key) && i.is_for(faction)) {
            if item.kind == "Cube" && !self.cube_places.is_empty() {
                self.gather.extend(self.cube_places.iter().map(|&(x, y, color)| (item, x, y, color)));
                continue;
            }
            for &(x, y) in gather::points_of(&map.table, &item.key) {
                self.gather.push((item, x, y, gather_color(&item.kind)));
            }
        }
        let chosen: HashSet<i32> = pet_ids.iter().copied().collect();
        self.pets = pets::points_of(&map.table, &chosen);
        let mut on_map: Vec<i32> = self.pets.iter().map(|p| p.0).collect::<HashSet<_>>().into_iter().collect();
        on_map.sort_unstable();
        for (i, id) in on_map.into_iter().enumerate() {
            self.pet_colors.insert(id, PET_PALETTE[i % PET_PALETTE.len()]);
        }
    }
}

pub struct LiveInputs<'a> {
    pub known: bool,
    pub player: (f32, f32),
    pub player_z: Option<f32>,
    pub env: &'a [EnvObject],
    pub mobs: &'a [LiveMob],
    pub show_live_mobs: bool,
    pub show_cubes: bool,
    pub pet_ids: &'a BTreeSet<i32>,
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
    let static_size = if view.radius_metres > 1500.0 { 4.0 } else if view.radius_metres > 500.0 { 6.0 } else { 8.0 };

    let push = |m: Marker, out: &mut Vec<Marker>| {
        if out.len() < MAX_MARKERS {
            let (sx, sy) = view.to_screen(map, m.x as f64, m.y as f64);
            out.push(Marker { x: sx as f32, y: sy as f32, ..m });
        }
    };

    for &(id, x, y) in &layer.pets {
        if in_view(x, y) {
            let color = layer.pet_colors.get(&id).copied().unwrap_or((255, 255, 255));
            push(Marker { x, y, color, size: static_size, live: false, level: 0 }, out);
        }
    }

    for &(item, x, y, color) in &layer.gather {
        if !in_view(x, y) {
            continue;
        }
        if near_player(x, y) && !live_env_matches(&item.key, x, y) {
            continue;
        }
        push(Marker { x, y, color, size: static_size, live: false, level: 0 }, out);
    }

    if live.known {
        for e in live.env {
            let wanted = if e.kind == "Cube" { live.show_cubes } else { layer.gather.iter().any(|g| g.0.key == e.key) };
            if wanted && in_view(e.x, e.y) {
                let size = if e.kind == "Cube" { 14.0 } else { 11.0 };
                let color = if e.kind == "Cube" { layer.cube_color_near(e.x, e.y).unwrap_or_else(|| gather_color("Cube")) } else { gather_color(&e.kind) };
                push(Marker { x: e.x, y: e.y, color, size, live: true, level: level_of(e.z, live.player_z) }, out);
            }
        }
        for &LiveMob { npc, x, y, .. } in live.mobs {
            if !in_view(x, y) {
                continue;
            }
            let pet = pets::pets_of_npc(npc).iter().find(|p| live.pet_ids.contains(p));
            match pet {
                Some(p) => {
                    let color = layer.pet_colors.get(p).copied().unwrap_or((255, 255, 255));
                    push(Marker { x, y, color, size: 13.0, live: true, level: 0 }, out);
                }
                None if live.show_live_mobs => push(Marker { x, y, color: (0xE0, 0x50, 0x50), size: 7.0, live: true, level: 0 }, out),
                None => {}
            }
        }
    }
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
    fn level_arrows_need_both_heights() {
        assert_eq!(level_of(27_000.0, Some(26_000.0)), 1);
        assert_eq!(level_of(25_000.0, Some(26_000.0)), -1);
        assert_eq!(level_of(26_400.0, Some(26_000.0)), 0);
        assert_eq!(level_of(f32::NAN, Some(26_000.0)), 0);
        assert_eq!(level_of(27_000.0, None), 0);
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
        let on_disk = std::fs::read_dir(map.tile_folder()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with("_a.png")).count();
        assert_eq!(shown, on_disk);
        visible_tiles(map, &view, &mut loader);
        assert!(!loader.loading(), "a settled view must not request tiles again");
        assert!(loader.bytes <= TILE_CACHE_BYTES);
    }
}
