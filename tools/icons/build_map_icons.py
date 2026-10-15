"""Builds the icons za2 shows on the map and in its lists, from textures `retoc to-legacy` converted.

    python3 build_map_icons.py <legacy dir> <linux/assets>

<legacy dir> holds the converted UI textures (see tools/README.md: UT_Vehicle_Portrait_, UT_256_MOB_, Icon_Gather_, GatherSkill_,
Icon_BMShop_Express_EmpyreanTrace, P_Img_HiddenCube_01, UT_HUD_Minimap_, Atlas_HUD_Minimap). Writes into <linux/assets>:
  map/icons/*.png + index.json   one icon per resource kind, the Hidden Cube and the minimap monster markers by rank
  pets/icons/<pet key>.png       the pet portraits (UT_Vehicle_Portrait_<key>)
  npcs/icons/<model>.png         a portrait per monster model: its own (UT_256_MOB_<model>) or the pet of that model
and adds "icon" to the monsters of npcs/monsters.json that have a portrait.
"""
import json
import os
import re
import sys

from PIL import Image, ImageChops, ImageDraw

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import legacy_texture  # noqa: E402

GATHER = {
    "Ore": "Icon_Gather_Ore_L_lv1_01a",
    "Herb": "Icon_Gather_Herb_L_lv1_01a",
    "Wood": "Icon_Gather_Wood_L_lv1_01a",
    "Cotton": "Icon_Gather_Cotton_L_lv1_01a",
    "Food": "Icon_Gather_Food_L_lv1_01c",
    "Od": "Icon_Gather_Od_L_lv4_01a",
    "Gemstone": "GatherSkill_Jewelry_01",
    "Fragment": "Icon_BMShop_Express_EmpyreanTrace_001",
}
# skill icons drawn on an opaque square backdrop: made round and see-through like the Icon_Gather_ ones
ROUND = {"Gemstone"}
CUBE = "P_Img_HiddenCube_01"
# minimap markers of hostile monsters (the game tints the plain ones; these are already red)
MARKERS = {"mob": "Monster_Aggressive", "elite": "Monster_Elite_Aggressive", "boss": "Monster_Boss_Aggressive"}
ICON_SIZE = 64
PORTRAIT_SIZE = 48


def index_textures(root):
    found = {}
    for folder, _, files in os.walk(root):
        for f in files:
            if f.endswith(".uexp"):
                found[f[:-5]] = os.path.join(folder, f)
    return found


def roundify(image):
    """The dark backdrop becomes transparent (the glow keeps its brightness as opacity), cut to the inscribed circle."""
    image = image.convert("RGBA")
    big = image.width * 4
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).ellipse((0, 0, big - 1, big - 1), fill=255)
    mask = mask.resize(image.size, Image.LANCZOS)
    r, g, b = image.convert("RGB").split()
    bright = ImageChops.lighter(ImageChops.lighter(r, g), b).point(lambda v: min(255, max(0, (v - 40) * 2)))
    image.putalpha(ImageChops.multiply(ImageChops.multiply(image.getchannel("A"), bright), mask))
    return image


def save(image, path, size):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    image.resize((size, size), Image.LANCZOS).save(path, optimize=True)


def main():
    legacy, assets = sys.argv[1:3]
    textures = index_textures(legacy)
    index = {"gather": {}}

    for kind, name in GATHER.items():
        if name in textures:
            image = legacy_texture.decode(textures[name])
            save(roundify(image) if kind in ROUND else image, f"{assets}/map/icons/{kind.lower()}.png", ICON_SIZE)
            index["gather"][kind] = f"map/icons/{kind.lower()}.png"
    if CUBE in textures:
        save(legacy_texture.decode(textures[CUBE]), f"{assets}/map/icons/cube.png", ICON_SIZE)
        index["cube"] = "map/icons/cube.png"

    atlas = legacy_texture.pages(textures["Atlas_HUD_Minimap"])
    for key, sprite in MARKERS.items():
        path = textures.get(f"UT_HUD_Minimap_{sprite}_Sprite")
        rect = path and legacy_texture.sprite_rect(path, atlas[0].size)
        if rect:
            crop = atlas[0].crop(rect)
            side = max(crop.size)
            square = Image.new("RGBA", (side, side))
            square.alpha_composite(crop, ((side - crop.width) // 2, (side - crop.height) // 2))
            save(square, f"{assets}/map/icons/{key}.png", ICON_SIZE // 2)
            index[key] = f"map/icons/{key}.png"
    json.dump(index, open(f"{assets}/map/icons/index.json", "w"), indent=1, sort_keys=True)

    pets = json.load(open(f"{assets}/pets/pets.json", encoding="utf-8"))["pets"]
    pet_icons = 0
    for pet in pets.values():
        name = f"UT_Vehicle_Portrait_{pet['key']}"
        if name in textures:
            save(legacy_texture.decode(textures[name]), f"{assets}/pets/icons/{pet['key']}.png", ICON_SIZE)
            pet_icons += 1

    monsters_path = f"{assets}/npcs/monsters.json"
    monsters = json.load(open(monsters_path))
    portraits = {}
    for npc, info in monsters.items():
        model = info.get("model", "")
        if not model.startswith("MOB_"):
            continue
        base = model[4:]
        source = next((textures[n] for n in (f"UT_256_{model}", f"UT_Vehicle_Portrait_{base}",
                                              f"UT_Vehicle_Portrait_{re.sub(r'_V[0-9]+$', '', base)}") if n in textures), None)
        if not source:
            continue
        file = f"npcs/icons/{model}.png"
        if model not in portraits:
            save(legacy_texture.decode(source), f"{assets}/{file}", PORTRAIT_SIZE)
            portraits[model] = file
        info["icon"] = file
    json.dump(monsters, open(monsters_path, "w"), separators=(",", ":"), sort_keys=True)
    with_icon = sum(1 for m in monsters.values() if "icon" in m)
    print(f"map icons {len(index['gather'])} kinds + {len(index) - 1} others, {pet_icons} pet portraits, "
          f"{len(portraits)} monster portraits ({with_icon} of {len(monsters)} monsters)")


if __name__ == "__main__":
    main()
