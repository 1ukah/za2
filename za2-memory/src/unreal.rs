//! The engine's own bookkeeping, found by what it contains rather than by code patterns: the game's code is
//! encrypted by NCGuard and decrypted page by page while it runs, so byte signatures in it are unreliable.
//! Field offsets come from the engine's reflection data, by name, so they follow game patches.

use std::collections::HashMap;

use crate::process::{Process, Region};

const NAME_BLOCKS_MAX: u32 = 8192;
const OBJECTS_PER_CHUNK: u64 = 65536;
/// Size of one entry of the object list: object, flags, cluster root, serial number.
const OBJECT_ITEM: u64 = 24;
/// A scan reads data regions in pieces this large, overlapping by the longest structure it looks for.
const SCAN_CHUNK: usize = 16 << 20;
const SCAN_OVERLAP: usize = (NAME_BLOCKS_MAX as usize + 4) * 8;

/// Offsets inside engine objects. UObject: class, name, outer. UStruct: super struct, first property. FField
/// (properties): next, name, offset of the value.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub class: u64,
    pub name: u64,
    pub outer: u64,
    pub super_struct: u64,
    pub child_properties: u64,
    pub field_next: u64,
    pub field_name: u64,
    pub property_offset: u64,
}

/// Unreal Engine 5.4 and later, then the older FField layout with a 16-byte owner.
const LAYOUTS: [Layout; 2] = [
    Layout { class: 0x10, name: 0x18, outer: 0x20, super_struct: 0x40, child_properties: 0x50, field_next: 0x18, field_name: 0x20, property_offset: 0x44 },
    Layout { class: 0x10, name: 0x18, outer: 0x20, super_struct: 0x40, child_properties: 0x50, field_next: 0x20, field_name: 0x28, property_offset: 0x4c },
];

pub fn plausible_pointer(v: u64) -> bool {
    (0x10000..1 << 47).contains(&v)
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// Writable data of the game executable, where the engine keeps its globals.
pub fn image_data(process: &Process, base: u64) -> Result<Vec<Region>, String> {
    let header = process.read_vec(base, 0x400).map_err(|e| format!("cannot read the game executable: {e}"))?;
    if &header[..2] != b"MZ" {
        return Err("the game executable has no MZ header".into());
    }
    let pe = u32_at(&header, 0x3c) as usize;
    if pe + 0x58 > header.len() {
        return Err("the game executable has no PE header".into());
    }
    let size = u32_at(&header, pe + 24 + 56) as u64;
    let regions = process.regions().map_err(|e| format!("cannot list the game's memory: {e}"))?;
    Ok(regions.into_iter().filter(|r| r.readable() && r.writable() && r.start >= base && r.end <= base + size).collect())
}

/// Calls `visit(data, offset)` for every 8-byte aligned offset of the regions, `data` being the chunk that holds it.
fn scan_regions(process: &Process, regions: &[Region], mut visit: impl FnMut(&[u8], usize, u64) -> bool) -> bool {
    for r in regions {
        let mut at = r.start;
        while at < r.end {
            let len = ((r.end - at) as usize).min(SCAN_CHUNK + SCAN_OVERLAP);
            let Ok(data) = process.read_vec(at, len) else {
                at += SCAN_CHUNK as u64;
                continue;
            };
            let own = len.min(SCAN_CHUNK);
            for offset in (0..own).step_by(8) {
                if visit(&data, offset, at + offset as u64) {
                    return true;
                }
            }
            at += SCAN_CHUNK as u64;
        }
    }
    false
}

/// The name table: a lock, the current block and byte cursor, then the block pointers. The first block starts with
/// the names every engine registers first.
fn find_name_blocks(process: &Process, data: &[Region]) -> Option<u64> {
    let mut found = None;
    scan_regions(process, data, |chunk, i, addr| {
        if i + 24 > chunk.len() {
            return false;
        }
        let current = u32_at(chunk, i + 8);
        let cursor = u32_at(chunk, i + 12);
        if current >= NAME_BLOCKS_MAX || cursor == 0 || cursor > 1 << 20 {
            return false;
        }
        let blocks = i + 16;
        let needed = blocks + 8 * (current as usize + 2);
        if needed > chunk.len() || !plausible_pointer(u64_at(chunk, blocks)) || u64_at(chunk, blocks + 8 * (current as usize + 1)) != 0 {
            return false;
        }
        if !(0..=current as usize).all(|k| plausible_pointer(u64_at(chunk, blocks + 8 * k))) {
            return false;
        }
        let mut head = [0u8; 20];
        if process.read(u64_at(chunk, blocks), &mut head).is_ok() && &head[2..6] == b"None" && &head[8..20] == b"ByteProperty" {
            found = Some(addr + 16);
            return true;
        }
        false
    });
    found
}

/// Candidates for the object list: chunk table, preallocated objects, max and current object count, max and
/// current chunk count, all consistent with 65,536 objects per chunk.
fn object_list_candidates(process: &Process, data: &[Region]) -> Vec<u64> {
    let mut found = Vec::new();
    scan_regions(process, data, |chunk, i, addr| {
        if i + 32 > chunk.len() || !plausible_pointer(u64_at(chunk, i)) {
            return false;
        }
        let at = |k: usize| u32_at(chunk, i + 16 + 4 * k) as i32 as i64;
        let (max_objects, objects, max_chunks, chunks) = (at(0), at(1), at(2), at(3));
        let per = OBJECTS_PER_CHUNK as i64;
        let consistent = (1..2000).contains(&chunks)
            && (chunks..5000).contains(&max_chunks)
            && objects > 1000
            && objects > (chunks - 1) * per
            && objects <= chunks * per
            && max_objects >= objects
            && max_objects <= max_chunks * per
            && max_objects > (max_chunks - 1) * per;
        if consistent {
            found.push(addr);
        }
        false
    });
    found
}

pub struct Unreal {
    pub process: Process,
    name_blocks: u64,
    objects: u64,
    pub layout: Layout,
    names: HashMap<u32, String>,
    supers: HashMap<u64, u64>,
    /// UClass objects by name, as `Package.Name` (for example `/Script/Engine.Actor`).
    classes: HashMap<String, u64>,
}

impl Unreal {
    pub fn discover(process: Process, base: u64) -> Result<Unreal, String> {
        let data = image_data(&process, base)?;
        let name_blocks = find_name_blocks(&process, &data).ok_or("the engine's name table was not found")?;
        let mut unreal = Unreal { process, name_blocks, objects: 0, layout: LAYOUTS[0], names: HashMap::new(), supers: HashMap::new(), classes: HashMap::new() };
        if unreal.fname(0).as_deref() != Some("None") {
            return Err("the engine's name table does not start with None".into());
        }
        unreal.objects = object_list_candidates(&process, &data)
            .into_iter()
            .find(|&candidate| {
                unreal.objects = candidate;
                let first = unreal.object_at(0);
                first.is_some_and(|o| unreal.object_name(o).as_deref() == Some("/Script/CoreUObject") && unreal.class_name(o).as_deref() == Some("Package"))
            })
            .ok_or("the engine's object list was not found")?;
        unreal.index_classes()?;
        let actor = unreal.class("/Script/Engine.Actor").ok_or("the class Actor was not found")?;
        unreal.layout = LAYOUTS
            .into_iter()
            .find(|&layout| {
                unreal.layout = layout;
                unreal.property(actor, "RootComponent").is_some_and(|o| (0x40..0x1000).contains(&o))
            })
            .ok_or("the engine's property layout is not one this reader knows")?;
        Ok(unreal)
    }

    pub fn fname(&mut self, index: u32) -> Option<String> {
        if let Some(name) = self.names.get(&index) {
            return Some(name.clone());
        }
        let block = self.process.u64(self.name_blocks + 8 * (index >> 16) as u64).filter(|&b| plausible_pointer(b))?;
        let entry = block + 2 * (index & 0xffff) as u64;
        let header = self.process.u32(entry)? as u16;
        let (wide, len) = (header & 1 != 0, (header >> 6) as usize);
        let bytes = self.process.read_vec(entry + 2, if wide { len * 2 } else { len }).ok()?;
        let name = if wide {
            String::from_utf16_lossy(&bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
        } else {
            bytes.iter().map(|&b| b as char).collect()
        };
        if self.names.len() > 200_000 {
            self.names.clear();
        }
        self.names.insert(index, name.clone());
        Some(name)
    }

    pub fn object_count(&self) -> u64 {
        self.process.u32(self.objects + 20).unwrap_or(0) as u64
    }

    pub fn object_at(&self, index: u64) -> Option<u64> {
        let chunks = self.process.u64(self.objects)?;
        let chunk = self.process.u64(chunks + 8 * (index / OBJECTS_PER_CHUNK))?;
        self.process.u64(chunk + OBJECT_ITEM * (index % OBJECTS_PER_CHUNK)).filter(|&o| plausible_pointer(o))
    }

    /// Every live object, read chunk by chunk.
    pub fn all_objects(&self) -> Vec<u64> {
        let count = self.object_count();
        let Some(chunks) = self.process.u64(self.objects) else { return Vec::new() };
        let mut out = Vec::with_capacity(count as usize);
        for c in 0..count.div_ceil(OBJECTS_PER_CHUNK) {
            let n = (count - c * OBJECTS_PER_CHUNK).min(OBJECTS_PER_CHUNK);
            let Some(chunk) = self.process.u64(chunks + 8 * c) else { continue };
            let Ok(items) = self.process.read_vec(chunk, (n * OBJECT_ITEM) as usize) else { continue };
            out.extend(items.chunks(OBJECT_ITEM as usize).map(|item| u64_at(item, 0)).filter(|&o| plausible_pointer(o)));
        }
        out
    }

    pub fn class_of(&self, object: u64) -> Option<u64> {
        self.process.u64(object + self.layout.class).filter(|&c| plausible_pointer(c))
    }

    pub fn outer_of(&self, object: u64) -> Option<u64> {
        self.process.u64(object + self.layout.outer).filter(|&o| plausible_pointer(o))
    }

    pub fn object_name(&mut self, object: u64) -> Option<String> {
        let index = self.process.u32(object + self.layout.name)?;
        self.fname(index)
    }

    pub fn class_name(&mut self, object: u64) -> Option<String> {
        let class = self.class_of(object)?;
        self.object_name(class)
    }

    pub fn super_of(&mut self, class: u64) -> Option<u64> {
        if let Some(&s) = self.supers.get(&class) {
            return (s != 0).then_some(s);
        }
        let s = self.process.u64(class + self.layout.super_struct).filter(|&s| plausible_pointer(s)).unwrap_or(0);
        self.supers.insert(class, s);
        (s != 0).then_some(s)
    }

    /// Whether `class` is `base` or derives from it.
    pub fn derives(&mut self, mut class: u64, base: u64) -> bool {
        for _ in 0..64 {
            if class == base {
                return true;
            }
            match self.super_of(class) {
                Some(s) => class = s,
                None => return false,
            }
        }
        false
    }

    /// Names of the class and all its parents, nearest first.
    pub fn class_chain(&mut self, mut class: u64) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..64 {
            out.push(self.object_name(class).unwrap_or_default());
            match self.super_of(class) {
                Some(s) => class = s,
                None => break,
            }
        }
        out
    }

    fn index_classes(&mut self) -> Result<(), String> {
        let objects = self.all_objects();
        let first_classes: Vec<u64> = objects.iter().take(64).filter_map(|&o| self.class_of(o)).collect();
        let class_class = first_classes.into_iter().find(|&c| self.object_name(c).as_deref() == Some("Class")).ok_or("the class Class was not found")?;
        let headers = self.process.read_many(&objects, 0x28);
        for (&object, header) in objects.iter().zip(headers) {
            let Some(header) = header else { continue };
            if u64_at(&header, self.layout.class as usize) != class_class {
                continue;
            }
            let outer = u64_at(&header, self.layout.outer as usize);
            let (Some(name), Some(package)) = (self.fname(u32_at(&header, self.layout.name as usize)), self.object_name(outer)) else { continue };
            if package.starts_with("/Script/") {
                self.classes.insert(format!("{package}.{name}"), object);
            }
        }
        Ok(())
    }

    pub fn class(&self, path: &str) -> Option<u64> {
        self.classes.get(path).copied()
    }

    /// Offset of the property `name` in objects of `class`, searching the class and its parents.
    pub fn property(&mut self, mut class: u64, name: &str) -> Option<u64> {
        let l = self.layout;
        for _ in 0..64 {
            let mut field = self.process.u64(class + l.child_properties).unwrap_or(0);
            let mut guard = 0;
            while plausible_pointer(field) && guard < 10_000 {
                guard += 1;
                let index = self.process.u32(field + l.field_name)?;
                if self.fname(index).as_deref() == Some(name) {
                    return self.process.u32(field + l.property_offset).map(|o| o as u64);
                }
                field = self.process.u64(field + l.field_next).unwrap_or(0);
            }
            class = self.super_of(class)?;
        }
        None
    }
}
