use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::assets;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameLayout {
    pub length_offset: usize,
    pub length_size: usize,
    pub little_endian: bool,
    pub length_includes_header: bool,
    pub header_size: usize,
    pub opcode_offset: usize,
    pub opcode_size: usize,
    pub max_frame_length: usize,
    #[serde(default = "fixed")]
    pub length_encoding: String,
    #[serde(default)]
    pub length_bias: i64,
    #[serde(default)]
    pub opcode_big_endian: bool,
}

fn fixed() -> String {
    "fixed".into()
}

impl Default for FrameLayout {
    fn default() -> Self {
        FrameLayout {
            length_offset: 0,
            length_size: 2,
            little_endian: true,
            length_includes_header: true,
            header_size: 4,
            opcode_offset: 2,
            opcode_size: 2,
            max_frame_length: 65535,
            length_encoding: fixed(),
            length_bias: 0,
            opcode_big_endian: false,
        }
    }
}

impl FrameLayout {
    pub fn is_varint(&self) -> bool {
        self.length_encoding.eq_ignore_ascii_case("varint")
    }

    pub fn body_min(&self) -> usize {
        (self.opcode_offset + self.opcode_size).max(1)
    }

    pub fn opcode_at(&self, data: &[u8], at: usize) -> u32 {
        if self.opcode_big_endian {
            (data[at] as u32) << 8 | data[at + 1] as u32
        } else {
            (data[at + 1] as u32) << 8 | data[at] as u32
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpcodeFamily {
    Unknown,
    Damage,
    Dot,
    Heal,
    HpUpdate,
    Nickname,
    Roster,
    Character,
    Equipment,
    Skills,
    Daevanion,
    Species,
    PetProgress,
    PetLevel,
    PetAdded,
    InventoryChange,
    TargetSelect,
    LocalPosition,
    Titles,
    TitleChange,
    Attributes,
    Stigmas,
    SkillBar,
    Inspect,
    Appearance,
    PlayerList,
    Session,
    Kill,
    Avoid,
    NpcSpawn,
    NpcRemove,
    EnvSpawn,
    EnvRemove,
    Zone,
}

impl OpcodeFamily {
    fn parse(name: &str) -> Option<Self> {
        use OpcodeFamily::*;
        let all = [
            Damage, Dot, Heal, HpUpdate, Nickname, Roster, Character, Equipment, Skills, Daevanion, Species, PetProgress, PetLevel,
            PetAdded, InventoryChange, TargetSelect, LocalPosition, Titles, TitleChange, Attributes, Stigmas, SkillBar, Inspect,
            Appearance, PlayerList, Session, Kill, Avoid, NpcSpawn, NpcRemove, EnvSpawn, EnvRemove, Zone,
        ];
        all.into_iter().find(|f| format!("{f:?}").eq_ignore_ascii_case(name))
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolDocument {
    #[serde(default)]
    calibrated: bool,
    game_version: Option<String>,
    server_ports: Option<Vec<u16>>,
    damage_layout: Option<String>,
    nickname_layout: Option<String>,
    dot_layout: Option<String>,
    hp_layout: Option<String>,
    bundle_opcode: Option<u32>,
    sync_opcodes: Option<Vec<u32>>,
    frame: Option<FrameLayout>,
    opcodes: Option<HashMap<String, Vec<u32>>>,
}

#[derive(Clone, Debug, Default)]
pub struct Protocol {
    pub calibrated: bool,
    pub game_version: String,
    pub server_ports: Vec<u16>,
    pub damage_layout: String,
    pub nickname_layout: String,
    pub dot_layout: String,
    pub hp_layout: String,
    pub bundle_opcode: Option<u32>,
    pub sync_opcodes: HashSet<u32>,
    pub frame: FrameLayout,
    families: HashMap<u32, OpcodeFamily>,
}

impl Protocol {
    pub fn load() -> Protocol {
        std::fs::read_to_string(assets::path("protocol/opcodes.json"))
            .ok()
            .and_then(|text| Protocol::from_json(&text))
            .unwrap_or_default()
    }

    pub fn from_json(json: &str) -> Option<Protocol> {
        let doc: ProtocolDocument = serde_json::from_str(json).ok()?;
        let mut families = HashMap::new();
        for (name, opcodes) in doc.opcodes.unwrap_or_default() {
            if let Some(family) = OpcodeFamily::parse(&name) {
                for opcode in opcodes {
                    families.insert(opcode, family);
                }
            }
        }

        Some(Protocol {
            calibrated: doc.calibrated,
            game_version: doc.game_version.unwrap_or_default(),
            server_ports: doc.server_ports.unwrap_or_default(),
            damage_layout: doc.damage_layout.unwrap_or_default(),
            nickname_layout: doc.nickname_layout.unwrap_or_default(),
            dot_layout: doc.dot_layout.unwrap_or_default(),
            hp_layout: doc.hp_layout.unwrap_or_default(),
            bundle_opcode: doc.bundle_opcode,
            sync_opcodes: doc.sync_opcodes.unwrap_or_default().into_iter().collect(),
            frame: doc.frame.unwrap_or_default(),
            families,
        })
    }

    pub fn family_of(&self, opcode: u32) -> OpcodeFamily {
        self.families.get(&opcode).copied().unwrap_or(OpcodeFamily::Unknown)
    }
}
