use std::collections::HashSet;

use za2_core::decoder::DamageEvent;
use za2_core::engine::Engine;
use za2_core::protocol::{FrameLayout, Protocol};
use za2_core::reassembler::TcpReassembler;
use za2_core::segment::TcpSegment;

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn record(body: &[u8], wire: &mut Vec<u8>) {
    let mut length = body.len() + 4;
    while length >= 0x80 {
        wire.push((length & 0x7f | 0x80) as u8);
        length >>= 7;
    }
    wire.push(length as u8);
    wire.extend_from_slice(body);
}

fn wire(hexes: &[&str]) -> Vec<u8> {
    let mut w = Vec::new();
    for h in hexes {
        record(&hex(h), &mut w);
    }
    w
}

const T0: i64 = 1_790_000_000_000_000;

fn segment(seq: u32, payload: Vec<u8>) -> TcpSegment {
    segment_at(T0, seq, payload)
}

fn segment_at(timestamp: i64, seq: u32, payload: Vec<u8>) -> TcpSegment {
    TcpSegment {
        timestamp,
        source: "10.0.0.1:7777".parse().unwrap(),
        destination: "192.168.0.2:50000".parse().unwrap(),
        sequence: seq,
        payload,
        from_server: true,
    }
}

fn engine() -> Engine {
    let protocol = Protocol::load();
    assert!(protocol.calibrated, "opcodes.json not found or not calibrated");
    Engine::new(protocol)
}

fn decode(engine: &mut Engine, seg: TcpSegment) -> Vec<DamageEvent> {
    engine.ingest(&seg);
    engine.last_events().to_vec()
}

#[test]
fn real_capture_damage_frames() {
    let frames: [(&str, i64, bool, bool, &str); 4] = [
        ("04388484030600be08fe26a8004f02000002433baf4101000000ba4ffd020100", 381, false, false, "Keen Strike"),
        ("04388484032600be08fe26a8005703000002433baf4101000000ba4fcc0401020100", 588, true, false, "Keen Strike"),
        ("04388484032600be080e4ea8004b02000002837dbe4101000000ba4fb00301010100", 432, false, false, "Rupture Strike"),
        ("0438be080400be0857fcb2004a020792ea4501000000ba4f410100", 65, false, true, "Blood Absorption"),
    ];
    let mut e = engine();
    assert!(e.decoder.protocol().server_ports.contains(&13328));
    let events = decode(&mut e, segment(1000, wire(&frames.map(|f| f.0))));
    assert_eq!(events.len(), frames.len());
    for (ev, f) in events.iter().zip(frames.iter()) {
        assert_eq!((ev.amount, ev.is_critical, ev.is_heal, ev.skill.as_str()), (f.1, f.2, f.3, f.4));
    }
    let entities = &e.decoder.entities;
    assert!(entities.name_for(events[0].source).unwrap().starts_with("Player #"));
    assert_eq!(entities.class_of(events[0].source), Some("Gladiator"));
}

#[test]
fn real_bundle_frame() {
    let bundle = "ffff08020000ff13200438b81c0400ec1b5e7d14011a02c3f8006c01000000c24e87080100200438f81b1d000150c40701001e1d0015003a0015c43a000c1b002700c51b004f200438ea53000212cd53001fea530007071b000c53001fe153000212b353001fe1530007071b00095300f100332a38f81b011335ade5cc0ae02e0001006065a020f4a0019e00f0090c5e7d1401004f576fc60aa7724600c021440d0e92f81b012e0090160538f81b09ec1b354a0120c00b2a0080332a38ea1b0113391f000f4d001415ea4d0010ea4d0010394d00118d77004f332a38e19a001c15e14d0010e14d00019a0011854d00b00e0036857120f4a0010000";
    let mut e = engine();
    assert_eq!(e.decoder.protocol().bundle_opcode, Some(0xFFFF));
    let events = decode(&mut e, segment(9000, wire(&[bundle])));
    assert_eq!(events.iter().map(|e| e.amount).collect::<Vec<_>>(), [1031, 964, 973, 947]);
    assert_eq!(events.iter().map(|e| e.source).collect::<HashSet<_>>().len(), 1);
}

#[test]
fn damage_frames_without_damage_block() {
    let frames: [(&str, Option<i64>, bool, &str); 9] = [
        ("0438E1AD010600C522E0B7F80011020000028BD3276101000000A85ADA3E0100", Some(8026), false, "Elemental Fusion"),
        ("0438C5220000C522E2B7F8000F0253D4276101000000A85A0100", None, false, "Elemental Fusion"),
        ("0438C5220000C522E0B7F800110295D3276102000000A85A0200", None, false, "Elemental Fusion"),
        ("0438A5C7012600C522102DF90036030000014B9A556101000000A85A8D1701A7020100", Some(2957), true, "Dimensional Control"),
        ("0438E1AD010000C522102DF90014024C9A556101000000A85A0100", None, false, "Dimensional Control"),
        ("0438A9A6010400C522E046F6009D038BAF336001000000A85AA50F0100", Some(1957), true, "Jointstrike: Curse"),
        ("0438A9A6010000C522E046F6009D028DAF336001000000A85A0100", None, false, "Jointstrike: Curse"),
        ("0438A9A6010000C52240C0F40072020D199B5F01000000A85A0100", None, false, "Combustion"),
        ("043885AE010400D92440C0F40009020B199B5F010000009656E7040100", Some(615), false, "Combustion"),
    ];
    let mut e = engine();
    let events = decode(&mut e, segment(5000, wire(&frames.map(|f| f.0))));
    let hits: Vec<_> = frames.iter().filter(|f| f.1.is_some()).collect();
    assert_eq!(events.len(), hits.len());
    for (ev, f) in events.iter().zip(hits) {
        assert_eq!((Some(ev.amount), ev.is_critical, ev.skill.as_str(), ev.is_heal), (f.1, f.2, f.3, false));
    }
}

#[test]
fn summoned_spirits_credited_to_summoner() {
    let water_spawn = "4136A5AE011F1000C18E2C0000020028A04500788245008031440E86B1437AFC01DF28DF2819070000190700000000000000000000000000005892010064000000F04902000100000000000000A08601000000000090D00300010111010F329A09FFFFFFFFFFFFFFFF8075D52ABB0300008E5509022B5A9B45CADA7B4590C525440702068E2A000002CD00C4040000D0003D0100001E000000E31D030000";
    let fire_spawn = "4136B7DD011F1000B28E2C00400200E8E44500F06B4500806C443AAF7D4366B40198639863630B0000630B000000000000000000000000000060B2010064000000F04902000100000000000000A08601000000000000E20400010101110144AA9809FFFFFFFFFFFFFFFF8075D52ABB030000CF190E02FB71E7451296784571BE6044070206CF0C000002CD005A000000D000300100002D000000DD1D030000";
    let water_hit = "0438EC91010600A5AE01BB86010002020000024FA09800010000009E5A690100";
    let water_spawn_heal = "0438A5AE010400A5AE01343F030101025CB04465010000009E5AB0DB060100";
    let fire_crit = "0438EC91012600B7DD01AE8601000503000002D99A980001000000C0528B0501410100";
    let reused = water_spawn.replace("8075D52ABB0300008E55", "8075D52ABB030000A5AE01");

    let mut e = engine();
    let first = wire(&[water_spawn, fire_spawn, water_hit, water_spawn_heal, fire_crit]);
    let first_len = first.len() as u32;
    let events = decode(&mut e, segment(7000, first));
    assert_eq!(e.decoder.entities.summon_owner_of(22309), Some(10894));
    assert_eq!(e.decoder.entities.summon_owner_of(28343), Some(3279));
    assert_eq!(events.len(), 2);
    assert!(!events.iter().any(|d| d.is_heal));
    assert_eq!((events[0].source, events[0].amount), (10894, 105));
    assert_eq!((events[1].source, events[1].amount, events[1].is_critical), (3279, 651, true));

    let after = decode(&mut e, segment(7000 + first_len, wire(&[&reused, water_hit])));
    assert_eq!(e.decoder.entities.summon_owner_of(22309), None);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].source, 22309);
}

#[test]
fn damage_over_time_ticks() {
    let frames: [(&str, Option<i64>, i32, i32, Option<&str>); 8] = [
        ("0538D490020AC522108BAF3360A804E046F600", Some(552), 4421, 34900, Some("Jointstrike: Curse")),
        ("0538D490020AD924248BAF3360C901E046F600", Some(201), 4697, 34900, Some("Jointstrike: Curse")),
        ("0538C5220AD49002F402E71327076AE0771B00", Some(106), 34900, 4421, None),
        ("0538833A0B833A91015FB2FC0BFF03AA01DDAF1E00", None, 0, 0, None),
        ("0538D92409833A240BED006CBA02407D1401", None, 0, 0, None),
        ("0538D9240A833A490BED006C4D407D1401", None, 0, 0, None),
        ("0438D9240400833A407D140105020BED006C010000008256B4020100", None, 0, 0, None),
        ("0538C52208C522EC0195D32761E2B7F800", None, 0, 0, None),
    ];
    let mut e = engine();
    let events = decode(&mut e, segment(6000, wire(&frames.map(|f| f.0))));
    assert_eq!(events.iter().filter(|e| e.is_heal && !e.is_tick && e.skill == "Recuperation" && e.amount == 308).count(), 1);
    let damage: Vec<_> = events.iter().filter(|e| !e.is_heal).collect();
    let ticks: Vec<_> = frames.iter().filter(|f| f.1.is_some()).collect();
    assert_eq!(damage.len(), ticks.len());
    for (ev, f) in damage.iter().zip(ticks) {
        assert_eq!((Some(ev.amount), ev.source, ev.target, ev.is_tick), (f.1, f.2, f.3, true));
        if let Some(skill) = f.4 {
            assert_eq!(ev.skill, skill);
        }
    }

    let hot = ["0538862D09833A240BED006CCE02407D1401", "0538862D0B833A240BED006CFB0153407D1401", "0538862D0B833A240BED006CA80153407D1401"];
    let mut hot_wire = Vec::new();
    for h in hot {
        let body = hex(h);
        hot_wire.push((body.len() + 4) as u8);
        hot_wire.extend(body);
    }
    let mut e = engine();
    let heals = decode(&mut e, segment(8000, hot_wire));
    assert_eq!(heals.len(), 2);
    assert!(heals.iter().all(|e| e.is_heal && e.is_tick && e.amount == 83 && e.skill == "Recuperation"));
}

#[test]
fn ticks_add_to_total_but_not_hits() {
    let ev = |s: i64, amount: i64, crit: bool, tick: bool| DamageEvent {
        timestamp: T0 + s * 1_000_000,
        source: 4421,
        target: 34900,
        amount,
        is_heal: false,
        skill: "Jointstrike: Curse".into(),
        is_critical: crit,
        is_tick: tick,
        skill_id: 0,
        kinds: None,
        hit_count: 1,
        estimated: false,
    };
    let curse = [ev(0, 1739, true, false), ev(10, 1957, false, false), ev(1, 552, false, true), ev(2, 552, false, true), ev(11, 528, false, true)];
    let usage = za2_core::combat::skill_breakdown(curse.iter(), false);
    assert_eq!(usage.len(), 1);
    let u = &usage[0];
    assert_eq!((u.hits, u.crit_hits, u.total, u.min, u.max), (2, 1, 1739 + 1957 + 552 + 552 + 528, 1739, 1957));
}

#[test]
fn hit_points_and_boss_reset() {
    let frames: [(f64, &str); 6] = [
        (0.0, "008DCE8D0102010008DD010000000000"),
        (34.2, "008DCE8D01020100F481010000000000"),
        (34.8, "008DCE8D0102010078E0010000000000"),
        (56.8, "008DCE8D0102010093DD010000000000"),
        (57.0, "008DCF19030201900A000003D89E01000100BD24000000000000"),
        (57.1, "008DCF19030508CD0700000A60B201000BF04902000CA08601000D00E2040001075B1B000000000000"),
    ];
    let mut e = engine();
    let mut seq = 4000u32;
    for (seconds, h) in frames {
        let body = hex(h);
        let mut rec = vec![(body.len() + 4) as u8];
        rec.extend(body);
        let len = rec.len() as u32;
        e.ingest(&segment_at(T0 + (seconds * 1e6) as i64, seq, rec));
        seq += len;
    }
    let hp = &e.decoder.entities.hit_points;
    let berk = 18126;
    assert_eq!(hp.samples(berk).iter().map(|s| s.hp).collect::<Vec<_>>(), [122_120, 98_804, 123_000, 122_259]);
    assert_eq!(hp.highest_seen(berk), Some(123_000));
    assert_eq!(hp.resets_of(berk), [T0 + 34_800_000]);
    assert_eq!(hp.samples(3279).iter().map(|s| s.hp).collect::<Vec<_>>(), [9_405]);
}

#[test]
fn capture_starting_mid_connection() {
    let bodies = [
        "04388484030600be08fe26a8004f02000002433baf4101000000ba4ffd020100",
        "04388484032600be08fe26a8005703000002433baf4101000000ba4fcc0401020100",
        "04388484032600be080e4ea8004b02000002837dbe4101000000ba4fb00301010100",
        "0438be080400be0857fcb2004a020792ea4501000000ba4f410100",
    ];
    let mut w = Vec::new();
    for _ in 0..6 {
        w.extend(wire(&bodies));
    }
    let mut results = Vec::new();
    for cut in [0usize, 3, 11, 25, 40] {
        let mut e = engine();
        results.push(decode(&mut e, segment(7000, w[cut..].to_vec())).len());
    }
    assert_eq!(results[0], 24);
    assert!(results[1..].iter().all(|&n| n >= 18), "{results:?}");
}

#[test]
fn character_record_names_local_player() {
    let body = "3336cb025fa1c12837044161687a18050600000001210000000000000000000000b6ec460f8ac1900600010000000000000000000000000003000000000000000000000000990eb788890c000200000000000000000000000000030000000000000000000000000e570f8b0c00030000000000000000000000000003000000000000000000000000dd0e777b860c000400000000000000000000000000030000000000000000000000000e1702880c00050000000000000000000000000003000000000000000000000000dd0efe958c0c000600000000000000000000000000030000000000000000000000000e961c8e0c00070000000000000000000000000003000000000000000000000000dd0e36a38f0c000800000000000000000000000000030000000000000000000000000e7e5c7c1200090000000000000000000000000003000000000000000000000000dd0e2e0a7e12000a00000000000000000000000000030000000000000000000000000e2c0a7e12000b0000000000000000000000000003000000000000000000000000dd0e0000000000";
    let mut e = engine();
    decode(&mut e, segment(300, wire(&[body])));
    let c = e.decoder.entities.local_character().cloned().unwrap();
    assert_eq!((c.name.as_str(), c.level, c.class_code, c.combat_id), ("Aahz", 33, 6, 331));
    assert_eq!(e.decoder.entities.local_player_id(), Some(331));
    assert_eq!(e.decoder.entities.name_for(331).as_deref(), Some("Aahz"));
}

fn fixed_frame(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(4 + body.len());
    f.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
    f.extend_from_slice(&opcode.to_le_bytes());
    f.extend_from_slice(body);
    f
}

#[test]
fn tcp_reassembly_reorder_retransmit_split() {
    let layout = FrameLayout { header_size: 4, max_frame_length: 4096, ..FrameLayout::default() };
    let sync = HashSet::new();
    let a = fixed_frame(1, &[0xAA, 0xBB, 0xCC, 0xDD]);
    let b = fixed_frame(2, &[0xEE, 0xFF]);
    let stream: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
    let (first, middle, last) = (stream[..5].to_vec(), stream[5..9].to_vec(), stream[9..].to_vec());

    let mut r = TcpReassembler::new();
    let mut frames: Vec<Vec<u8>> = Vec::new();
    r.push(&segment(1000, first.clone()), &layout, &sync, &mut |f| frames.push(f.to_vec()));
    assert!(frames.is_empty());
    r.push(&segment(1009, last), &layout, &sync, &mut |f| frames.push(f.to_vec()));
    assert!(frames.is_empty());
    r.push(&segment(1005, middle), &layout, &sync, &mut |f| frames.push(f.to_vec()));
    assert_eq!(frames, [a, b]);

    frames.clear();
    r.push(&segment(1000, first), &layout, &sync, &mut |f| frames.push(f.to_vec()));
    assert!(frames.is_empty());
    assert_eq!(r.retransmissions, 1);

    let c = fixed_frame(3, &[1, 2, 3, 4, 5, 6]);
    let mut seq = 1000 + stream.len() as u32;
    for piece in [&c[..2], &c[2..5], &c[5..]] {
        r.push(&segment(seq, piece.to_vec()), &layout, &sync, &mut |f| frames.push(f.to_vec()));
        seq += piece.len() as u32;
    }
    assert_eq!(frames, [c]);
    assert_eq!(r.frames, 3);
}

#[test]
fn varint_stream_with_loss_realigns() {
    let bodies = [
        "04388484030600be08fe26a8004f02000002433baf4101000000ba4ffd020100",
        "04388484032600be08fe26a8005703000002433baf4101000000ba4fcc0401020100",
    ];
    let mut w = Vec::new();
    for _ in 0..200 {
        w.extend(wire(&bodies));
    }
    let mut e = engine();
    let chunk = 50;
    let mut total = 0;
    let mut seq = 100u32;
    for (i, piece) in w.chunks(chunk).enumerate() {
        if i != 3 {
            total += decode(&mut e, segment(seq, piece.to_vec())).len();
        }
        seq += piece.len() as u32;
    }
    assert!(e.reassembler.gaps >= 1);
    assert!(total >= 390 && total < 400, "{total}");
}

fn zone_frame(zone: u32) -> String {
    let mut body = vec![0x21, 0x36, 1, 0, 0, 0];
    body.extend_from_slice(&zone.to_le_bytes());
    body.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn zone_change_closes_the_session() {
    let hits = [
        "04388484030600be08fe26a8004f02000002433baf4101000000ba4ffd020100",
        "04388484032600be08fe26a8005703000002433baf4101000000ba4fcc0401020100",
    ];
    let mut e = engine();
    let first = wire(&[&zone_frame(1010)]);
    let mut seq = 500u32;
    e.ingest(&segment_at(T0, seq, first.clone()));
    seq += first.len() as u32;
    let fight = wire(&hits);
    e.ingest(&segment_at(T0 + 200_000_000, seq, fight.clone()));
    seq += fight.len() as u32;
    assert!(e.drain_finished_sessions().is_empty());
    e.ingest(&segment_at(T0 + 400_000_000, seq, wire(&[&zone_frame(600001)])));
    let finished = e.drain_finished_sessions();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].zone_id, 1010);
    assert_eq!(finished[0].total(), 381 + 588);
    assert!(e.combat.events().is_empty());
}
