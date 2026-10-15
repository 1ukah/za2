use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::Value;

use crate::assets::read_json;

fn object_entries(value: Option<&Value>) -> impl Iterator<Item = (&String, &Value)> {
    value.and_then(Value::as_object).into_iter().flat_map(|m| m.iter())
}

fn names_of(value: &Value) -> HashMap<String, String> {
    object_entries(value.get("names"))
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

pub fn localized<'a>(names: &'a HashMap<String, String>, language: &str) -> Option<&'a str> {
    names.get(language).or_else(|| names.get("en")).map(String::as_str)
}

pub mod skills {
    use super::*;

    const CLASS_BY_PREFIX: [&str; 9] = ["Gladiator", "Templar", "Assassin", "Ranger", "Sorcerer", "Spiritmaster", "Cleric", "Chanter", "Brawler"];
    const CLASS_BY_ID: [&str; 8] = ["Gladiator", "Templar", "Ranger", "Assassin", "Spiritmaster", "Sorcerer", "Cleric", "Chanter"];

    fn names() -> &'static HashMap<i32, String> {
        static NAMES: OnceLock<HashMap<i32, String>> = OnceLock::new();
        NAMES.get_or_init(|| {
            object_entries(read_json("skills/skill_names.json").as_ref())
                .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_string())))
                .collect()
        })
    }

    fn heal_bases() -> &'static HashSet<i32> {
        static SET: OnceLock<HashSet<i32>> = OnceLock::new();
        SET.get_or_init(|| {
            read_json("skills/heal_skill_families.json")
                .and_then(|doc| doc.get("heal").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_i64().map(|i| i as i32))
                .collect()
        })
    }

    fn buff_bases() -> &'static HashSet<i32> {
        static SET: OnceLock<HashSet<i32>> = OnceLock::new();
        SET.get_or_init(|| {
            read_json("skills/rdps_watchlist.json")
                .and_then(|doc| doc.get("buffs").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .filter(|b| b.get("kind").and_then(Value::as_str) != Some("target_debuff"))
                .filter_map(|b| b.get("skillId").and_then(Value::as_i64).map(|id| id as i32 / 10000 * 10000))
                .collect()
        })
    }

    pub fn name_of(skill_id: i32) -> String {
        let table = names();
        if let Some(exact) = table.get(&skill_id) {
            return exact.clone();
        }
        table.get(&(skill_id / 10000 * 10000)).cloned().unwrap_or_else(|| skill_id.to_string())
    }

    pub fn english_name(skill_id: i32) -> Option<&'static str> {
        let table = names();
        table.get(&skill_id).or_else(|| table.get(&(skill_id / 10000 * 10000))).map(String::as_str)
    }

    pub fn is_non_damage_effect(skill_id: i32) -> bool {
        (2_000_000..3_000_000).contains(&skill_id) || buff_bases().contains(&(skill_id / 10000 * 10000))
    }

    pub fn is_heal_family(skill_id: i32) -> bool {
        heal_bases().contains(&(skill_id / 10000 * 10000))
    }

    pub fn class_of(skill_id: i32) -> Option<&'static str> {
        let prefix = skill_id / 1_000_000;
        (skill_id >= 10_000_000 && (11..=19).contains(&prefix)).then(|| CLASS_BY_PREFIX[(prefix - 11) as usize])
    }

    pub fn is_summon_attack(skill_id: i32) -> bool {
        let name = name_of(skill_id);
        name == "Divine Aura" || name == "Bittercold Wind" || (name.contains("Spirit:") && !name.starts_with("Summon"))
    }

    pub fn summon_attack_class(skill_id: i32) -> Option<&'static str> {
        if !is_summon_attack(skill_id) {
            return None;
        }
        class_of(skill_id).or_else(|| name_of(skill_id).contains("Spirit:").then_some("Spiritmaster"))
    }

    pub fn class_from_code(code: i32) -> Option<&'static str> {
        let id = code / 4;
        (matches!(code % 4, 1 | 2) && (1..=8).contains(&id)).then(|| CLASS_BY_ID[(id - 1) as usize])
    }

    pub fn class_from_roster_code(code: i32) -> Option<&'static str> {
        let id = (code - 1) / 4;
        (code >= 1 && (1..=8).contains(&id)).then(|| CLASS_BY_ID[(id - 1) as usize])
    }
}

pub mod bosses {
    use super::*;

    pub struct BossInfo {
        pub name: String,
        pub instance: String,
    }

    fn table() -> &'static HashMap<i32, BossInfo> {
        static TABLE: OnceLock<HashMap<i32, BossInfo>> = OnceLock::new();
        TABLE.get_or_init(|| {
            object_entries(read_json("npcs/boss_npcs.json").as_ref().and_then(|d| d.get("bosses")))
                .filter_map(|(k, v)| {
                    Some((
                        k.parse().ok()?,
                        BossInfo {
                            name: v.get("name")?.as_str()?.to_string(),
                            instance: v.get("instance").and_then(Value::as_str).unwrap_or("").to_string(),
                        },
                    ))
                })
                .collect()
        })
    }

    pub fn find(npc_id: i32) -> Option<&'static BossInfo> {
        table().get(&npc_id)
    }

    fn field_boss_npcs() -> &'static HashSet<i32> {
        static SET: OnceLock<HashSet<i32>> = OnceLock::new();
        SET.get_or_init(|| {
            let doc = read_json("bosses/field_bosses.json");
            let mut set = HashSet::new();
            for map in doc.as_ref().and_then(|d| d.get("maps")).and_then(Value::as_array).into_iter().flatten() {
                for boss in map.get("bosses").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(npc) = boss.get("npc").and_then(Value::as_i64) {
                        set.insert(npc as i32);
                    }
                }
            }
            set
        })
    }

    pub fn is_estimated_boss(npc_id: i32) -> bool {
        field_boss_npcs().contains(&npc_id)
    }

    const MIN_SAMPLES: i64 = 8;

    struct Averages {
        skills: HashMap<i32, (i64, i64)>,
        families: HashMap<i32, (i64, i64)>,
    }

    fn averages() -> &'static Averages {
        static AVG: OnceLock<Averages> = OnceLock::new();
        AVG.get_or_init(|| {
            let doc = read_json("protocol/skill_hit_average.json");
            let read = |key: &str| -> HashMap<i32, (i64, i64)> {
                object_entries(doc.as_ref().and_then(|d| d.get(key)))
                    .filter_map(|(k, v)| Some((k.parse().ok()?, (v.get("avg")?.as_i64()?, v.get("n")?.as_i64()?))))
                    .collect()
            };
            Averages { skills: read("skills"), families: read("families") }
        })
    }

    pub fn average_hit(skill_id: i32) -> Option<i64> {
        let a = averages();
        if let Some(&(avg, n)) = a.skills.get(&skill_id) {
            if n >= MIN_SAMPLES {
                return Some(avg);
            }
        }
        a.families.get(&(skill_id / 10)).filter(|(_, n)| *n >= MIN_SAMPLES).map(|(avg, _)| *avg)
    }
}

pub mod npcs {
    use super::*;

    fn table() -> &'static HashMap<i32, HashMap<String, String>> {
        static TABLE: OnceLock<HashMap<i32, HashMap<String, String>>> = OnceLock::new();
        TABLE.get_or_init(|| {
            let text = std::fs::read_to_string(crate::assets::path("npcs/npc_names.json")).unwrap_or_default();
            let raw: HashMap<String, HashMap<String, String>> = serde_json::from_str(&text).unwrap_or_default();
            raw.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()
        })
    }

    pub fn name_of(npc_id: i32, language: &str) -> Option<&'static str> {
        table().get(&npc_id).and_then(|names| localized(names, language))
    }
}

pub mod zones {
    use super::*;

    struct Zone {
        instance: bool,
        names: HashMap<String, String>,
    }

    fn table() -> &'static HashMap<i32, Zone> {
        static TABLE: OnceLock<HashMap<i32, Zone>> = OnceLock::new();
        TABLE.get_or_init(|| {
            let doc = read_json("zones/zones.json");
            doc.as_ref()
                .and_then(|d| d.get("zones"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|z| {
                    Some((
                        z.get("id")?.as_i64()? as i32,
                        Zone { instance: z.get("kind").and_then(Value::as_str) == Some("instance"), names: names_of(z) },
                    ))
                })
                .collect()
        })
    }

    pub fn is_instance(zone_id: i32) -> bool {
        table().get(&zone_id).map(|z| z.instance).unwrap_or((600_000..700_000).contains(&zone_id))
    }

    pub fn name_of(zone_id: i32, language: &str) -> Option<&'static str> {
        table().get(&zone_id).and_then(|z| localized(&z.names, language))
    }
}

pub mod gather {
    use super::*;

    pub const KINDS: [&str; 9] = ["Od", "Herb", "Food", "Ore", "Wood", "Cotton", "Gemstone", "Fragment", "Cube"];
    const ELYOS_MAP: &str = "World/World_L/World_L_A";
    const ASMODIAN_MAP: &str = "World/World_D/World_D_A";

    #[derive(Clone, Debug)]
    pub struct GatherItem {
        pub kind: String,
        pub key: String,
        pub names: HashMap<String, String>,
        pub count: i64,
        pub elyos: usize,
        pub asmodian: usize,
    }

    impl GatherItem {
        pub fn name_in(&self, language: &str) -> &str {
            self.names.get(language).map(String::as_str).unwrap_or(&self.key)
        }

        pub fn is_for(&self, faction: Option<&str>) -> bool {
            match faction {
                Some("Elyos") => !(self.elyos == 0 && self.asmodian > 0),
                Some("Asmodian") => !(self.asmodian == 0 && self.elyos > 0),
                _ => true,
            }
        }
    }

    pub struct Gather {
        pub items: Vec<GatherItem>,
        key_by_id: HashMap<i64, (String, String)>,
        maps: HashMap<String, HashMap<String, Vec<(f32, f32)>>>,
        /// Hidden Cube spawn groups per map: the server keeps a few cubes of a group out and puts a new one at another place of the
        /// same group when one is taken.
        cube_groups: HashMap<String, Vec<Vec<(f32, f32)>>>,
    }

    pub fn data() -> &'static Gather {
        static DATA: OnceLock<Gather> = OnceLock::new();
        DATA.get_or_init(load)
    }

    fn load() -> Gather {
        let doc = read_json("maps/gather.json");
        let mut items: HashMap<(String, String), (HashMap<String, String>, i64)> = HashMap::new();
        let mut key_of: HashMap<String, (String, String)> = HashMap::new();
        let mut key_by_id = HashMap::new();
        let mut od_names: Option<HashMap<String, String>> = None;
        for (id, obj) in object_entries(doc.as_ref().and_then(|d| d.get("objects"))) {
            let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            let mut names = names_of(obj);
            if kind == "Od" {
                names = od_names.get_or_insert(names).clone();
            }
            let key = names.get("en").cloned().unwrap_or_else(|| id.clone());
            key_of.insert(id.clone(), (kind.clone(), key.clone()));
            if let Ok(env_id) = id.parse::<i64>() {
                key_by_id.insert(env_id, (kind.clone(), key.clone()));
            }
            let count = obj.get("count").and_then(Value::as_i64).unwrap_or(0);
            items.entry((kind, key)).and_modify(|e| e.1 += count).or_insert((names, count));
        }

        let mut maps: HashMap<String, HashMap<String, Vec<(f32, f32)>>> = HashMap::new();
        for (table, objects) in object_entries(doc.as_ref().and_then(|d| d.get("maps"))) {
            let per_item = maps.entry(table.clone()).or_default();
            for (id, points) in object_entries(Some(objects)) {
                let Some((_, key)) = key_of.get(id) else { continue };
                let list = per_item.entry(key.clone()).or_default();
                for p in points.as_array().into_iter().flatten() {
                    if let (Some(x), Some(y)) = (p.get(0).and_then(Value::as_f64), p.get(1).and_then(Value::as_f64)) {
                        list.push((x as f32, y as f32));
                    }
                }
            }
        }

        let side = |map: &str, key: &str| maps.get(map).and_then(|m| m.get(key)).map_or(0, Vec::len);
        let mut list: Vec<GatherItem> = items
            .into_iter()
            .map(|((kind, key), (names, count))| GatherItem {
                elyos: side(ELYOS_MAP, &key),
                asmodian: side(ASMODIAN_MAP, &key),
                kind,
                key,
                names,
                count,
            })
            .collect();
        list.sort_by(|a, b| {
            let ka = KINDS.iter().position(|k| *k == a.kind).map_or(-1, |p| p as i64);
            let kb = KINDS.iter().position(|k| *k == b.kind).map_or(-1, |p| p as i64);
            ka.cmp(&kb).then((b.count > 0).cmp(&(a.count > 0))).then(a.key.to_lowercase().cmp(&b.key.to_lowercase()))
        });
        let cube_groups = object_entries(doc.as_ref().and_then(|d| d.get("cubeGroups")))
            .map(|(table, groups)| {
                let list = groups
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|g| {
                        g.get("points")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|p| Some((p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32)))
                            .collect()
                    })
                    .collect();
                (table.clone(), list)
            })
            .collect();
        Gather { items: list, key_by_id, maps, cube_groups }
    }

    pub fn is_hidden_cube(env_id: i64) -> bool {
        matches!(env_id, 1200036..=1200041 | 1200667 | 1200707 | 1201502..=1201505 | 1300081..=1300083 | 1300754 | 1301199 | 1301200 | 1301242..=1301245 | 1100056 | 1100094)
    }

    pub fn lookup(env_id: i64) -> Option<(&'static str, &'static str)> {
        data().key_by_id.get(&env_id).map(|(k, key)| (k.as_str(), key.as_str()))
    }

    pub fn item(key: &str) -> Option<&'static GatherItem> {
        data().items.iter().find(|i| i.key == key)
    }

    pub fn cube_groups(table: &str) -> &'static [Vec<(f32, f32)>] {
        data().cube_groups.get(table).map_or(&[], Vec::as_slice)
    }

    pub fn points_of(table: &str, key: &str) -> &'static [(f32, f32)] {
        data().maps.get(table).and_then(|m| m.get(key)).map_or(&[], Vec::as_slice)
    }
}

pub mod pets {
    use super::*;

    pub const TOP_LEVEL: i32 = 3;

    pub struct Pets {
        pub names: HashMap<i32, HashMap<String, String>>,
        pets_of_npc: HashMap<i32, Vec<i32>>,
        spawns: HashMap<String, HashMap<i32, Vec<(f32, f32)>>>,
    }

    pub fn data() -> &'static Pets {
        static DATA: OnceLock<Pets> = OnceLock::new();
        DATA.get_or_init(|| {
            let doc = read_json("pets/pets.json");
            let names = object_entries(doc.as_ref().and_then(|d| d.get("pets")))
                .filter_map(|(k, v)| Some((k.parse().ok()?, names_of(v))))
                .collect();
            let mut pets_of_npc: HashMap<i32, Vec<i32>> = HashMap::new();
            for (k, v) in object_entries(doc.as_ref().and_then(|d| d.get("monsters"))) {
                let Ok(npc) = k.parse::<i32>() else { continue };
                let ids: Vec<i32> = match v {
                    Value::Array(a) => a.iter().filter_map(Value::as_i64).map(|i| i as i32).collect(),
                    other => other.as_i64().map(|i| vec![i as i32]).unwrap_or_default(),
                };
                pets_of_npc.entry(npc).or_default().extend(ids);
            }

            let spawn_doc = read_json("pets/spawns.json");
            let spawns = object_entries(spawn_doc.as_ref().and_then(|d| d.get("maps")))
                .map(|(table, npcs)| {
                    let per_npc = object_entries(Some(npcs))
                        .filter_map(|(npc, points)| {
                            let pts = points
                                .as_array()?
                                .iter()
                                .filter_map(|p| Some((p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32)))
                                .collect();
                            Some((npc.parse().ok()?, pts))
                        })
                        .collect();
                    (table.clone(), per_npc)
                })
                .collect();
            Pets { names, pets_of_npc, spawns }
        })
    }

    pub fn pets_of_npc(npc_id: i32) -> &'static [i32] {
        data().pets_of_npc.get(&npc_id).map_or(&[], Vec::as_slice)
    }

    pub fn pet_name(pet_id: i32, language: &str) -> Option<&'static str> {
        data().names.get(&pet_id).and_then(|n| localized(n, language))
    }

    pub fn spawn_table(table: &str) -> Option<&'static HashMap<i32, Vec<(f32, f32)>>> {
        data().spawns.get(table)
    }

    pub fn points_of(table: &str, pets: &HashSet<i32>) -> Vec<(i32, f32, f32)> {
        let mut result = Vec::new();
        let Some(spawns) = spawn_table(table) else { return result };
        if pets.is_empty() {
            return result;
        }
        for (npc, points) in spawns {
            for &pet in pets_of_npc(*npc) {
                if pets.contains(&pet) {
                    result.extend(points.iter().map(|&(x, y)| (pet, x, y)));
                }
            }
        }
        result
    }

    pub fn pets_on_map(table: &str) -> Vec<i32> {
        let mut set: Vec<i32> = spawn_table(table)
            .into_iter()
            .flat_map(|t| t.keys())
            .flat_map(|npc| pets_of_npc(*npc).iter().copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        set.sort_unstable();
        set
    }
}

pub mod maps {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub struct MapInfo {
        pub key: String,
        pub title: String,
        pub table: String,
        pub tile: u32,
        pub grid: u32,
        pub scale: f64,
        pub offset_x: f64,
        pub offset_y: f64,
    }

    impl MapInfo {
        pub fn to_native(&self, world_x: f64, world_y: f64) -> (f64, f64) {
            (self.offset_x + self.scale * world_x, self.offset_y + self.scale * world_y)
        }

        pub fn to_world(&self, native_x: f64, native_y: f64) -> (f64, f64) {
            ((native_x - self.offset_x) / self.scale, (native_y - self.offset_y) / self.scale)
        }

        pub fn native_size(&self) -> f64 {
            (self.tile * self.grid) as f64
        }

        pub fn tile_folder(&self) -> std::path::PathBuf {
            crate::assets::path("maps").join(&self.key)
        }
    }

    pub fn all() -> &'static [MapInfo] {
        static ALL: OnceLock<Vec<MapInfo>> = OnceLock::new();
        ALL.get_or_init(|| {
            let doc = read_json("maps/maps.json");
            let mut list: Vec<MapInfo> = object_entries(doc.as_ref().and_then(|d| d.get("maps")))
                .filter_map(|(key, v)| {
                    Some(MapInfo {
                        key: key.clone(),
                        title: v.get("title").and_then(Value::as_str).unwrap_or(key).to_string(),
                        table: v.get("table")?.as_str()?.to_string(),
                        tile: v.get("tile")?.as_u64()? as u32,
                        grid: v.get("grid")?.as_u64()? as u32,
                        scale: v.get("scale")?.as_f64()?,
                        offset_x: v.get("offsetX")?.as_f64()?,
                        offset_y: v.get("offsetY")?.as_f64()?,
                    })
                })
                .collect();
            let order = |k: &str| ["verteron", "altgard", "abyss"].iter().position(|o| *o == k).unwrap_or(99);
            list.sort_by_key(|m| order(&m.key));
            list
        })
    }

    pub fn by_key(key: &str) -> Option<&'static MapInfo> {
        all().iter().find(|m| m.key == key)
    }

    /// The map of a game world, by the world's name as the engine has it (`World_D_A` for the table `World/World_D/World_D_A`).
    pub fn by_world(world: &str) -> Option<&'static MapInfo> {
        all().iter().find(|m| m.table.rsplit('/').next().is_some_and(|last| last.eq_ignore_ascii_case(world)))
    }

    fn near(points: &[(f32, f32)], x: f32, y: f32, limit: f32) -> bool {
        let limit2 = limit * limit;
        points.iter().any(|&(px, py)| {
            let (dx, dy) = (px - x, py - y);
            dx * dx + dy * dy <= limit2
        })
    }

    pub fn detect(mobs: &[(i32, f32, f32)]) -> Option<&'static MapInfo> {
        let mut votes: Vec<(usize, usize)> = Vec::new();
        for &(npc, x, y) in mobs {
            for (index, map) in all().iter().enumerate() {
                if let Some(points) = pets::spawn_table(&map.table).and_then(|t| t.get(&npc)) {
                    if near(points, x, y, 4000.0) {
                        match votes.iter_mut().find(|v| v.0 == index) {
                            Some(v) => v.1 += 1,
                            None => votes.push((index, 1)),
                        }
                    }
                }
            }
        }
        let mut best: Option<(usize, usize)> = None;
        for &(index, count) in &votes {
            if best.map_or(true, |b| count > b.1) {
                best = Some((index, count));
            }
        }
        best.map(|b| &all()[b.0])
    }
}
