//! Line protocol between `za2-memory` (reads the game's memory) and the meter.
//!
//! Helper to meter, one message per line:
//! - `status <text>`
//! - `self <x> <y> <z> <yaw> <world>`: own character, yaw in degrees (0 = +x, 90 = +y), world as in `World_D_A`
//! - `noself`: no own character (loading screen, character select)
//! - `npc <actor> <entity> <x> <y> <z>`: an NPC actor; actor is its address in hex, entity the server id (0 = unknown)
//! - `frame`: the NPC list since the previous `frame` is complete
//! - `found <actor> <entity> <offset>`: answer to `probe`, the entity id sits at this offset in the actor
//!
//! Meter to helper:
//! - `probe <actor> <entity>`: search the actor for this server id
//! - `idoffset <offset>`: where NPC actors keep their server id (0 = unknown)

#[derive(Clone, Debug, PartialEq)]
pub struct SelfFix {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub world: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NpcActor {
    pub actor: u64,
    pub entity: i32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MemoryMessage {
    Status(String),
    SelfFix(SelfFix),
    NoSelf,
    Npc(NpcActor),
    Frame,
    Found { actor: u64, entity: i32, offset: usize },
}

impl MemoryMessage {
    pub fn to_line(&self) -> String {
        match self {
            MemoryMessage::Status(text) => format!("status {}", text.replace('\n', " ")),
            MemoryMessage::SelfFix(f) => format!("self {} {} {} {} {}", f.x, f.y, f.z, f.yaw, f.world.as_deref().unwrap_or("-")),
            MemoryMessage::NoSelf => "noself".into(),
            MemoryMessage::Npc(n) => format!("npc {:x} {} {} {} {}", n.actor, n.entity, n.x, n.y, n.z),
            MemoryMessage::Frame => "frame".into(),
            MemoryMessage::Found { actor, entity, offset } => format!("found {actor:x} {entity} {offset}"),
        }
    }

    pub fn parse(line: &str) -> Option<MemoryMessage> {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut it = rest.split_whitespace();
        let mut f32_next = || it.next()?.parse::<f32>().ok().filter(|v| v.is_finite());
        Some(match kind {
            "status" => MemoryMessage::Status(rest.to_string()),
            "noself" => MemoryMessage::NoSelf,
            "frame" => MemoryMessage::Frame,
            "self" => {
                let (x, y, z, yaw) = (f32_next()?, f32_next()?, f32_next()?, f32_next()?);
                let world = rest.split_whitespace().nth(4).filter(|w| *w != "-").map(str::to_string);
                MemoryMessage::SelfFix(SelfFix { x, y, z, yaw, world })
            }
            "npc" => {
                let mut it = rest.split_whitespace();
                let actor = u64::from_str_radix(it.next()?, 16).ok()?;
                let entity = it.next()?.parse().ok()?;
                let mut num = || it.next()?.parse::<f32>().ok().filter(|v| v.is_finite());
                MemoryMessage::Npc(NpcActor { actor, entity, x: num()?, y: num()?, z: num()? })
            }
            "found" => {
                let mut it = rest.split_whitespace();
                MemoryMessage::Found { actor: u64::from_str_radix(it.next()?, 16).ok()?, entity: it.next()?.parse().ok()?, offset: it.next()?.parse().ok()? }
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MemoryCommand {
    Probe { actor: u64, entity: i32 },
    IdOffset(usize),
}

impl MemoryCommand {
    pub fn to_line(&self) -> String {
        match self {
            MemoryCommand::Probe { actor, entity } => format!("probe {actor:x} {entity}"),
            MemoryCommand::IdOffset(offset) => format!("idoffset {offset}"),
        }
    }

    pub fn parse(line: &str) -> Option<MemoryCommand> {
        let mut it = line.split_whitespace();
        match it.next()? {
            "probe" => Some(MemoryCommand::Probe { actor: u64::from_str_radix(it.next()?, 16).ok()?, entity: it.next()?.parse().ok()? }),
            "idoffset" => Some(MemoryCommand::IdOffset(it.next()?.parse().ok()?)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let all = [
            MemoryMessage::Status("found the game, pid 16424".into()),
            MemoryMessage::SelfFix(SelfFix { x: -60622.285, y: -73038.65, z: 27230.05, yaw: -45.625, world: Some("World_D_A".into()) }),
            MemoryMessage::SelfFix(SelfFix { x: 1.0, y: 2.0, z: 3.0, yaw: 0.0, world: None }),
            MemoryMessage::NoSelf,
            MemoryMessage::Npc(NpcActor { actor: 0x1f9abc010, entity: 33049, x: -64412.0, y: -78775.5, z: 26000.0 }),
            MemoryMessage::Frame,
            MemoryMessage::Found { actor: 0x1f9abc010, entity: 33049, offset: 0x26f0 },
        ];
        for m in all {
            assert_eq!(MemoryMessage::parse(&m.to_line()), Some(m.clone()), "{}", m.to_line());
        }
        for c in [MemoryCommand::Probe { actor: 0xf9408020, entity: 33984 }, MemoryCommand::IdOffset(9968)] {
            assert_eq!(MemoryCommand::parse(&c.to_line()), Some(c));
        }
        assert_eq!(MemoryMessage::parse("self 1 2 nan 4 W"), None);
        assert_eq!(MemoryMessage::parse("bogus"), None);
    }
}
