use std::collections::HashMap;
use std::time::{Duration, Instant};

use za2_core::memory_stream::{NpcActor, SelfFix};

use crate::unreal::{plausible_pointer, Unreal};

/// Where NPC actors kept the server's entity id in the build of 2026-10-15. The meter checks it against the
/// packets and searches again when it stops matching.
pub const DEFAULT_ID_OFFSET: usize = 0x26f0;
const PROBE_BYTES: usize = 0x4000;
const CONTROLLER_RETRY: Duration = Duration::from_secs(3);

struct Offsets {
    acknowledged_pawn: u64,
    root_component: u64,
    relative_location: u64,
    relative_rotation: u64,
    attach_parent: u64,
}

/// Where a scene component keeps its world transform (rotation quaternion, translation, scale). It is not a
/// reflected property, so it is found by matching the translation of a component that is attached to nothing.
#[derive(Clone, Copy)]
struct WorldTransform {
    translation: u64,
}

pub struct Reader {
    pub unreal: Unreal,
    offsets: Offsets,
    player_controller_class: u64,
    controller: Option<(u64, u64)>,
    controller_checked: Option<Instant>,
    world_transform: Option<WorldTransform>,
    worlds: HashMap<u64, Option<String>>,
    /// Per level: offset of its actor list.
    actor_lists: HashMap<u64, Option<u64>>,
    /// Actor to whether it is an NPC, for the actors in the current level.
    actor_kinds: HashMap<u64, bool>,
    class_kinds: HashMap<u64, bool>,
    pub id_offset: usize,
}

pub struct Frame {
    pub own: Option<SelfFix>,
    pub npcs: Vec<NpcActor>,
}

fn f64s(data: &[u8]) -> impl Iterator<Item = f64> + '_ {
    data.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap()))
}

impl Reader {
    pub fn new(mut unreal: Unreal) -> Result<Reader, String> {
        let class = |unreal: &Unreal, name: &str| unreal.class(&format!("/Script/Engine.{name}")).ok_or(format!("the class {name} was not found"));
        let (controller, actor, scene) = (class(&unreal, "PlayerController")?, class(&unreal, "Actor")?, class(&unreal, "SceneComponent")?);
        let mut property = |class: u64, name: &str| unreal.property(class, name).ok_or(format!("the property {name} was not found"));
        let offsets = Offsets {
            acknowledged_pawn: property(controller, "AcknowledgedPawn")?,
            root_component: property(actor, "RootComponent")?,
            relative_location: property(scene, "RelativeLocation")?,
            relative_rotation: property(scene, "RelativeRotation")?,
            attach_parent: property(scene, "AttachParent")?,
        };
        Ok(Reader {
            unreal,
            offsets,
            player_controller_class: controller,
            controller: None,
            controller_checked: None,
            world_transform: None,
            worlds: HashMap::new(),
            actor_lists: HashMap::new(),
            actor_kinds: HashMap::new(),
            class_kinds: HashMap::new(),
            id_offset: DEFAULT_ID_OFFSET,
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "pawn +{:#x}, root +{:#x}, location +{:#x}, world transform {}",
            self.offsets.acknowledged_pawn,
            self.offsets.root_component,
            self.offsets.relative_location,
            self.world_transform.map_or("not found yet".into(), |w| format!("+{:#x}", w.translation))
        )
    }

    /// The local player controller: the one live object that is a PlayerController and has a pawn.
    fn find_controller(&mut self) -> Option<(u64, u64)> {
        let objects = self.unreal.all_objects();
        let classes = self.unreal.process.read_many(&objects.iter().map(|o| o + self.unreal.layout.class).collect::<Vec<_>>(), 8);
        let mut fallback = None;
        for (&object, class) in objects.iter().zip(classes) {
            let Some(class) = class.map(|c| u64::from_le_bytes(c.try_into().unwrap())).filter(|&c| plausible_pointer(c)) else { continue };
            if !self.unreal.derives(class, self.player_controller_class) || self.unreal.object_name(object).is_some_and(|n| n.starts_with("Default__")) {
                continue;
            }
            if self.unreal.process.u64(object + self.offsets.acknowledged_pawn).is_some_and(plausible_pointer) {
                return Some((object, class));
            }
            fallback.get_or_insert((object, class));
        }
        fallback
    }

    fn controller(&mut self) -> Option<u64> {
        if let Some((object, class)) = self.controller {
            if self.unreal.class_of(object) == Some(class) {
                return Some(object);
            }
            self.controller = None;
        }
        if self.controller_checked.is_some_and(|at| at.elapsed() < CONTROLLER_RETRY) {
            return None;
        }
        self.controller_checked = Some(Instant::now());
        self.controller = self.find_controller();
        self.controller.map(|c| c.0)
    }

    fn find_world_transform(&mut self, root: u64) {
        let p = &self.unreal.process;
        if p.u64(root + self.offsets.attach_parent).is_some_and(plausible_pointer) {
            return;
        }
        let (Ok(component), Ok(relative)) = (p.read_vec(root, 0x400), p.read_vec(root + self.offsets.relative_location, 24)) else { return };
        // FTransform: quaternion (4 doubles), translation (3 doubles and padding), scale (3 doubles and padding).
        for at in (32..component.len() - 64).step_by(16) {
            if at as u64 == self.offsets.relative_location || component[at..at + 24] != relative[..] {
                continue;
            }
            let quaternion: Vec<f64> = f64s(&component[at - 32..at]).collect();
            let scale: Vec<f64> = f64s(&component[at + 32..at + 56]).collect();
            let norm = quaternion.iter().map(|q| q * q).sum::<f64>();
            if (norm - 1.0).abs() < 1e-3 && scale.iter().all(|s| (0.01..100.0).contains(s)) {
                self.world_transform = Some(WorldTransform { translation: at as u64 });
                return;
            }
        }
    }

    fn world_name(&mut self, pawn: u64) -> Option<String> {
        let level = self.unreal.outer_of(pawn)?;
        let world = self.unreal.outer_of(level)?;
        if let Some(name) = self.worlds.get(&world) {
            return name.clone();
        }
        let name = self.unreal.object_name(world);
        self.worlds.insert(world, name.clone());
        name
    }

    /// Location and yaw of a root component: from its world transform when known, else its relative values.
    fn place(&self, root: u64) -> Option<(f64, f64, f64, f64)> {
        let p = &self.unreal.process;
        if let Some(w) = self.world_transform {
            let data = p.read_vec(root + w.translation - 32, 56).ok()?;
            let v: Vec<f64> = f64s(&data).collect();
            let (x, y, z, w_) = (v[0], v[1], v[2], v[3]);
            let yaw = (2.0 * (w_ * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees();
            return Some((v[4], v[5], v[6], yaw));
        }
        let [x, y, z] = p.f64s::<3>(root + self.offsets.relative_location)?;
        let [_, yaw, _] = p.f64s::<3>(root + self.offsets.relative_rotation)?;
        Some((x, y, z, yaw))
    }

    fn own(&mut self, pawn: u64) -> Option<SelfFix> {
        let root = self.unreal.process.u64(pawn + self.offsets.root_component).filter(|&r| plausible_pointer(r))?;
        if self.world_transform.is_none() {
            self.find_world_transform(root);
        }
        let (x, y, z, yaw) = self.place(root)?;
        [x, y, z, yaw].iter().all(|v| v.is_finite()).then(|| SelfFix { x: x as f32, y: y as f32, z: z as f32, yaw: yaw as f32, world: self.world_name(pawn) })
    }

    fn actor_list(&mut self, level: u64, pawn: u64) -> Option<Vec<u64>> {
        let p = self.unreal.process;
        let offset = match self.actor_lists.get(&level) {
            Some(o) => *o,
            None => {
                // ULevel keeps its actors in an array that is not a reflected property: find the array that holds
                // the own pawn.
                let data = p.read_vec(level, 0x400).ok()?;
                let found = (0x28..data.len() - 16).step_by(8).find(|&at| {
                    let (items, num, max) = (u64::from_le_bytes(data[at..at + 8].try_into().unwrap()), i32::from_le_bytes(data[at + 8..at + 12].try_into().unwrap()), i32::from_le_bytes(data[at + 12..at + 16].try_into().unwrap()));
                    plausible_pointer(items)
                        && num > 0
                        && num <= max
                        && max <= 1_000_000
                        && p.read_vec(items, num as usize * 8).is_ok_and(|list| list.chunks_exact(8).any(|c| u64::from_le_bytes(c.try_into().unwrap()) == pawn))
                });
                let found = found.map(|o| o as u64);
                self.actor_lists.insert(level, found);
                found
            }
        }?;
        let items = p.u64(level + offset)?;
        let num = p.u32(level + offset + 8)? as usize;
        if num > 1_000_000 {
            return None;
        }
        let list = p.read_vec(items, num * 8).ok()?;
        Some(list.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).filter(|&a| plausible_pointer(a)).collect())
    }

    fn is_npc_class(&mut self, class: u64) -> bool {
        if let Some(&kind) = self.class_kinds.get(&class) {
            return kind;
        }
        let chain = self.unreal.class_chain(class);
        let kind = chain.iter().any(|n| n == "Character") && chain.iter().any(|n| n.contains("Npc"));
        self.class_kinds.insert(class, kind);
        kind
    }

    fn npcs(&mut self, pawn: u64) -> Vec<NpcActor> {
        let Some(level) = self.unreal.outer_of(pawn) else { return Vec::new() };
        let Some(actors) = self.actor_list(level, pawn) else { return Vec::new() };
        let p = self.unreal.process;
        let fresh: Vec<u64> = actors.iter().copied().filter(|a| !self.actor_kinds.contains_key(a)).collect();
        if !fresh.is_empty() {
            let classes = p.read_many(&fresh.iter().map(|a| a + self.unreal.layout.class).collect::<Vec<_>>(), 8);
            for (actor, class) in fresh.into_iter().zip(classes) {
                let class = class.map(|c| u64::from_le_bytes(c.try_into().unwrap())).filter(|&c| plausible_pointer(c));
                let npc = class.is_some_and(|c| self.is_npc_class(c));
                self.actor_kinds.insert(actor, npc);
            }
        }
        if self.actor_kinds.len() > actors.len() + 4096 {
            let live: std::collections::HashSet<u64> = actors.iter().copied().collect();
            self.actor_kinds.retain(|a, _| live.contains(a));
        }
        let npcs: Vec<u64> = actors.into_iter().filter(|a| self.actor_kinds.get(a) == Some(&true)).collect();
        let roots = p.read_many(&npcs.iter().map(|a| a + self.offsets.root_component).collect::<Vec<_>>(), 8);
        let ids = if self.id_offset > 0 { p.read_many(&npcs.iter().map(|a| a + self.id_offset as u64).collect::<Vec<_>>(), 4) } else { vec![None; npcs.len()] };
        let mut out = Vec::with_capacity(npcs.len());
        for ((actor, root), id) in npcs.into_iter().zip(roots).zip(ids) {
            let Some(root) = root.map(|r| u64::from_le_bytes(r.try_into().unwrap())).filter(|&r| plausible_pointer(r)) else { continue };
            let Some((x, y, z, _)) = self.place(root) else { continue };
            if ![x, y, z].iter().all(|v| v.is_finite() && v.abs() < 1e7) {
                continue;
            }
            let entity = id.map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()));
            out.push(NpcActor { actor, entity, x: x as f32, y: y as f32, z: z as f32 });
        }
        out
    }

    pub fn frame(&mut self) -> Frame {
        let pawn = self.controller().and_then(|c| self.unreal.process.u64(c + self.offsets.acknowledged_pawn)).filter(|&p| plausible_pointer(p));
        let Some(pawn) = pawn else { return Frame { own: None, npcs: Vec::new() } };
        Frame { own: self.own(pawn), npcs: self.npcs(pawn) }
    }

    /// Offsets inside `actor` that hold the 32-bit value `entity`.
    pub fn probe(&self, actor: u64, entity: i32) -> Vec<usize> {
        let pages: Vec<u64> = (0..PROBE_BYTES as u64).step_by(0x1000).map(|o| actor + o).collect();
        let mut out = Vec::new();
        for (page, data) in self.unreal.process.read_many(&pages, 0x1000).into_iter().enumerate() {
            let Some(data) = data else { continue };
            for at in (0..data.len()).step_by(4) {
                if i32::from_le_bytes(data[at..at + 4].try_into().unwrap()) == entity {
                    out.push(page * 0x1000 + at);
                }
            }
        }
        out
    }
}
