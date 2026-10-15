"""Lists the enemies of the game: every NpcData row of type Monster that is hostile to players, with its rank and creature type.

Usage:  python3 build_monsters.py <NpcData.bin from dectable.py> <npc_names.json> <out.json>

A row is [i32 npc id][internal name][STR_ text key][subtitle key][model]... followed by its enum values as text
(`ENpcType::Monster`, `ENpcSubType::EliteMonster`, `ECreatureType::Feral`, `ERelationshipEntity::Monster`, ...). Strings are
UTF-16 FStrings with a negative length, XOR-ed with 25 00 a8 00 7e 00 91 00 (the terminating null is not XOR-ed).
Kept: ENpcType::Monster whose relationship is Monster or a Monster team (not PC-friendly, not invisible helpers), with an English name.
Writes {npc id: {"rank": "Normal"|"Elite"|"Hero"|"Legend", "creature": "Feral"|..., "model": "MOB_Dracuni_01"}}; the model names
the monster's portrait (build_map_icons.py). build_spawns.py then keeps their spawns.
"""
import json
import struct
import sys

KEY = bytes([0x25, 0, 0xA8, 0, 0x7E, 0, 0x91, 0])
RANKS = {"NormalMonster": "Normal", "EliteMonster": "Elite", "HeroMonster": "Hero", "LegendMonster": "Legend"}
NOT_ENEMIES = {"Monster_PCFriendly", "Monster_Invisible"}


def rows(d):
    def string_at(q):
        if q + 4 > len(d):
            return None
        n = struct.unpack_from("<i", d, q)[0]
        if -200 < n < -1 and q + 4 - 2 * n <= len(d):
            raw = d[q + 4:q + 4 + 2 * (-n - 1)]
            text = bytes(b ^ KEY[j % 8] for j, b in enumerate(raw)).decode("utf-16le", "replace")
            if text.isprintable() and text.isascii():
                return text, q + 4 - 2 * n
        return None

    starts = []
    i = 0
    while i < len(d) - 8:
        npc = struct.unpack_from("<i", d, i)[0]
        if 100000 <= npc < 100000000 and -100 < struct.unpack_from("<i", d, i + 4)[0] < -2:
            first = string_at(i + 4)
            second = first and string_at(first[1])
            if second and second[0].startswith("STR_"):
                starts.append((i, npc))
                i = first[1]
                continue
        i += 1
    for k, (p, npc) in enumerate(starts):
        end = starts[k + 1][0] if k + 1 < len(starts) else len(d)
        texts = []
        q = p + 4
        while q < end:
            found = string_at(q)
            if found:
                texts.append(found[0])
                q = found[1]
            else:
                q += 1
        yield npc, texts


def main():
    table, names_path, out_path = sys.argv[1:4]
    names = json.load(open(names_path, encoding="utf-8"))
    out = {}
    for npc, texts in rows(open(table, "rb").read()):
        enum = {}
        for t in texts:
            if "::" in t:
                kind, _, value = t.partition("::")
                enum.setdefault(kind, value)
        if enum.get("ENpcType") != "Monster" or not enum.get("ERelationshipEntity", "").startswith("Monster"):
            continue
        if enum["ERelationshipEntity"] in NOT_ENEMIES or not names.get(str(npc), {}).get("en"):
            continue
        model = next((t for t in texts if t.startswith(("MOB_", "NPC_"))), None)
        out[str(npc)] = {"rank": RANKS.get(enum.get("ENpcSubType"), "Normal"), "creature": enum.get("ECreatureType", "None")}
        if model:
            out[str(npc)]["model"] = model
    json.dump(out, open(out_path, "w"), separators=(",", ":"), sort_keys=True)
    print(f"{len(out)} enemies -> {out_path}")


if __name__ == "__main__":
    main()
