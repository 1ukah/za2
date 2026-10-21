# Data tools

These tools make the game data and the icons in `assets/` from the files of an installed game client. za2 does not need them to build or run. Use them after a game update.

The tools read local files only. They do not send data to the network.

## Tools

| Tool | Function |
|---|---|
| `icons/keyscan/` | Finds the AES key of the game `.pak` files in the memory of the running game. |
| `icons/find_aes_key.py` | Finds the code that makes the AES key, in the game executable or in the game memory. Use `keyscan` first. |
| `dat/dectable.py` | Decrypts a data table (`.dat`). |
| `dat/extract_l10n.py` | Extracts the game texts in all languages. |
| `dat/build_npcs.py` | Makes `assets/npcs/npc_names.json`: the names of all NPCs. |
| `dat/build_monsters.py` | Makes `assets/npcs/monsters.json`: all enemies, with rank, creature type and model. |
| `dat/build_spawns.py` | Makes `assets/npcs/spawns.json`: the spawn points of all enemies and pet monsters on all maps. |
| `dat/build_pets.py` | Makes `assets/pets/pets.json`: the pets and the monsters that give them. |
| `dat/build_gather.py` | Makes `assets/maps/gather.json`: the gathering spots. |
| `dat/build_field_bosses.py` | Makes `assets/bosses/field_bosses.json`: the field bosses. |
| `dat/build_hidden_cubes.py` | Makes `hidden_cubes.json`: all possible places of the Hidden Cubes. za2 does not use this file. `gather.json` has the cube places. |
| `dat/build_kibelisks.py` | Makes `assets/maps/kibelisks.json`: the Kibelisks (teleport artifacts) of the world maps. |
| `dat/build_soul_routes.py` | Makes `soul_places.json`: the places that the pet window shows for each pet soul. `build_pet_regions.py` uses it. |
| `dat/build_pet_regions.py` | Makes `assets/pets/regions.json`: the named regions of the world maps and their pets. |
| `dat/build_pet_instances.py` | Makes `assets/pets/instances.json`: the names of the maps with pet monsters, other than the world maps. |
| `dat/build_skill_i18n.py` | Adds the monster, item and other skills to `assets/skills/skill_names_i18n.json`. |
| `dat/build_species.py` | Makes `assets/species/species_stats.json`: the stats of the species knowledge. |
| `dat/build_titles.py` | Makes `assets/titles/titles.json`: the titles in all languages. |
| `dat/build_wings.py` | Makes `wings.json`: the wings and their stats. |
| `dat/build_item_stats.py` | Makes `item_stats.json`: the names of the stats on equipment. za2 does not use this file. |
| `dat/build_tickets.py` | Makes `assets/tickets/tickets.json`: the entry tickets and their caps. |
| `icons/dat.py` | Reads the text strings of a decrypted table. The other `icons/` scripts use it. |
| `icons/legacy_texture.py` | Decodes a game texture to an image. The other `icons/` scripts use it. |
| `icons/build_map_icons.py` | Makes the icons of resources, Hidden Cubes, monsters and pets. |
| `icons/build_skill_icons.py` | Makes `assets/skills/skill_icon_tokens.json`: the icon name of each skill. |
| `icons/build_item_map.py` | Makes `assets/items/item_icons.json`: the icon name of each item. |
| `icons/build_nodes.py` | Makes `assets/daevanion/nodes.json`: the nodes of the Daevanion boards. |
| `icons/convert_textures.py` | Writes converted textures as PNG files of one size. |
| `recording/` | Reads recordings of game traffic. See [recording/README.md](recording/README.md). |
| `maps/build_maptiles.py` | Makes the map tiles: whole tiles, as the in-game map shows them. `--mask` cuts the sea away (the old output, which also cut away some land). |
| `maps/export_client_maps.py` | Exports the map tiles and `assets/maps/maps.json` for za2. |

## Requirements

- Python 3 with `Pillow`:

  ```sh
  python3 -m venv .venv
  .venv/bin/pip install Pillow
  ```

- `openssl`. The tools use it to decrypt.
- A Rust toolchain. You need it for `keyscan`.
- [repak](https://github.com/trumank/repak) to unpack the game `.pak` files:

  ```sh
  cargo install --git https://github.com/trumank/repak repak_cli
  ```

- [retoc](https://github.com/trumank/retoc) to extract the textures from the IoStore files (`.utoc` and `.ucas`):

  ```sh
  cargo install --locked --git https://github.com/trumank/retoc --tag v0.1.5 retoc_cli
  ```

- `tools/dat/constants.local.json` with the two constants of the `.dat` format. They are in the public [CUE4Parse](https://github.com/FabianFG/CUE4Parse) sources (`FAion2KeyManifestFile._manifestHashKey`, `Aion2DatFileEncryption.HeaderXorConstant`). Git ignores this file.

  ```json
  { "manifest_hash_key_hex": "<hex>", "header_xor_constant": "0x<hex>" }
  ```

- The AES key of the game `.pak` files. The key is not in this repository. To get the key, do the procedure in [Get the AES key](#get-the-aes-key).

In the commands below, `<Paks>` is the folder `AION2/Aion2/Content/Paks` of the game. `<key>` is the AES key, in the form `0x` and 64 hexadecimal digits.

## Get the AES key

The game executable is encrypted on the disk. The key is only in the memory of the running game. `keyscan` reads the game memory. It tries each 32-byte block as the key on the index of a `.pak` file.

1. Start the game. Log in with a character.
2. Let your user read the memory of other processes:

   ```sh
   sudo sysctl kernel.yama.ptrace_scope=0
   ```

3. Build `keyscan`:

   ```sh
   cd tools/icons/keyscan
   cargo build --release
   ```

4. Find the process ID of the game. Use the `AION2.exe` process with the most memory:

   ```sh
   pgrep -af 'AION2.exe'
   ```

5. Run `keyscan`. It shows the key after approximately 10 seconds:

   ```sh
   target/release/keyscan <pid> <Paks>/pakchunk0-Windows_0_P.pak
   ```

6. Set the memory access back to the default:

   ```sh
   sudo sysctl kernel.yama.ptrace_scope=1
   ```

7. Make sure that the key is correct. This command must show a list of files:

   ```sh
   repak --aes-key <key> list <Paks>/pakchunk0-Windows_0_P.pak
   ```

Do not put the key in a file in the repository.

If `keyscan` does not find the key, try `icons/find_aes_key.py --pid <pid> --pak <Paks>/pakchunk0-Windows_0_P.pak`.

## Make the data files

1. Unpack the files from the game `Paks` folder:

   ```sh
   repak --aes-key <key> unpack -i "Aion2/Content/Data/Map/**" -i "Aion2/Content/Data/Table/*.dat" -o <out> <Paks>/pakchunk401000-Windows_0_P.pak
   repak --aes-key <key> unpack -i "Aion2/Content/StartUpData/Table/key_manifest.dat" -o <out> <Paks>/pakchunk0-Windows.pak
   ```

   Also unpack `L10N/Text/<locale>/L10NString.dat` from each pak in `Paks/L10N/Text/`. The scripts expect the folder name `AION2/`. A symbolic link from `AION2` to `Aion2` is sufficient.

2. Decrypt the tables and the texts:

   ```sh
   python3 tools/dat/dectable.py <out>/Aion2/Content/StartUpData/Table/key_manifest.dat <table>.dat <table>.bin
   python3 tools/dat/extract_l10n.py <out> <l10n dir>
   ```

   Do `dectable.py` for `NpcData`, `VehicleList`, `EnvObjData`, `WorldMapFieldNamed` and `Item`.

3. Make the data files. Do the scripts in this sequence, because some scripts use the output of a script above them:

   | Script | Output |
   |---|---|
   | `dat/build_npcs.py <NpcData.bin> <l10n dir> <out.json>` | `assets/npcs/npc_names.json` |
   | `dat/build_pets.py <VehicleList.bin> <NpcData.bin> <npc_names.json> <l10n dir> <out.json> dat/extra_pets.json [Item.bin]` | `assets/pets/pets.json` |
   | `dat/build_monsters.py <NpcData.bin> <npc_names.json> <out.json>` | `assets/npcs/monsters.json` |
   | `dat/build_spawns.py <out>/AION2/Content/Data/Map <monsters.json> <pets.json> <out.json>` | `assets/npcs/spawns.json` |
   | `dat/build_gather.py <out>/AION2/Content/Data/Map <EnvObjData.bin> <l10n dir> <out.json>` | `assets/maps/gather.json` |
   | `dat/build_field_bosses.py <NpcData.bin> <WorldMapFieldNamed.bin> <npc_names.json> <l10n dir> <out>/AION2/Content/Data/Map <out.json>` | `assets/bosses/field_bosses.json` |

An enemy is an NPC of type `Monster` that attacks players. `build_monsters.py` does not keep NPCs that help players and invisible helper NPCs.

### Other data files

These scripts make more data files. Decrypt the tables they need with `dectable.py` first (`Subzone`, `WorldMapUIRegion`, `ItemGetRoute`, `Title`, `Wing`, `DaevanionNode`, `Skill`).

| Script | Output |
|---|---|
| `dat/build_kibelisks.py <EnvObjData.bin> <out>/AION2/Content/Data/Map <l10n dir> <out.json>` | `assets/maps/kibelisks.json` |
| `dat/build_soul_routes.py <ItemGetRoute.bin> <Subzone.bin> <pets.json> <soul_places.json>` | `soul_places.json` (for the next script) |
| `dat/build_pet_regions.py <Subzone.bin> <WorldMapUIRegion.bin> <out>/AION2/Content/Data/Map <l10n dir> <pets.json> <spawns.json> <out.json> [soul_places.json]` | `assets/pets/regions.json` |
| `dat/build_pet_instances.py <spawns.json> <instances.json> <l10n dir> <out.json>` | `assets/pets/instances.json` |
| `dat/build_skill_i18n.py <l10n dir> assets/skills/skill_names_i18n.json` | Changes `assets/skills/skill_names_i18n.json` |
| `dat/build_species.py <l10n dir> <out.json>` | `assets/species/species_stats.json` |
| `dat/build_titles.py <Title.bin> <l10n dir> <out.json>` | `assets/titles/titles.json` |
| `dat/build_wings.py <Wing.bin> <l10n dir> <out.json>` | `wings.json` |
| `dat/build_item_stats.py <l10n dir> <out.json> [godstones out.json]` | `item_stats.json` |
| `dat/build_tickets.py <l10n dir> <contents_tickets.json> <out.json>` | `assets/tickets/tickets.json` |
| `icons/build_nodes.py <tables dir> assets/daevanion/nodes.json <out.json>` | `assets/daevanion/nodes.json` |

Some scripts need a file that is not in this repository:

- `build_pet_instances.py` needs the instance table of the website backend (`Backend/src/data/aion2/instances.json`). It uses this table only for party dungeons without a name text.
- `build_tickets.py` needs `contents_tickets.json`: a dump of the game table `ContentsTicket`. The tool that made this dump is not available. Do not run `build_tickets.py` until a tool reads this table.

## Make the icons

The icons are textures in the IoStore files. `retoc` converts them to the legacy format (`.uasset`, `.uexp` and, for large textures, `.ubulk`). `build_map_icons.py` decodes them and writes PNG files.

1. Convert the textures. Do this command one time for each name part in the table below:

   ```sh
   retoc -a <key> to-legacy --no-shaders --no-script-objects --version UE5_3 -f <name part> <Paks> <legacy dir>
   ```

   | Name part | Textures |
   |---|---|
   | `UT_Vehicle_Portrait_` | Pet portraits. A pet has the name of the monster model. |
   | `UT_256_MOB_` | Monster portraits. |
   | `Icon_Gather_` | Icons of gathered items. |
   | `GatherSkill_` | Icons of gathering skills. |
   | `Icon_BMShop_Express_EmpyreanTrace` | Icon of the Empyrean Traces. |
   | `P_Img_HiddenCube_01` | Image of the Hidden Cube. |
   | `UT_HUD_Minimap_` | Minimap markers (sprites). |
   | `Atlas_HUD_Minimap` | The texture of the minimap markers. |

2. Make the icons:

   ```sh
   python3 tools/icons/build_map_icons.py <legacy dir> assets
   ```

   The script writes:

   | Output | Content |
   |---|---|
   | `assets/map/icons/` and `index.json` | One icon for each resource kind, the Hidden Cube, and the minimap markers of normal, elite and boss monsters. |
   | `assets/pets/icons/<pet key>.png` | The pet portraits. |
   | `assets/npcs/icons/<model>.png` | The monster portraits. |
   | `assets/npcs/monsters.json` | The script adds the portrait to each monster that has one. |

   Do `build_monsters.py` before this step. The script changes `monsters.json`.

To find other textures, list the files of all IoStore containers:

```sh
for u in <Paks>/*.utoc; do retoc -a <key> list --path "$u"; done > iostore-files.txt
```

### Skill icons

`assets/skills/skill_icon_tokens.json` gives the icon name (token) for the first four digits of a skill ID. `assets/skills/icons/<token>.png` holds the icons at 48 px.

1. Make a list of the IoStore files:

   ```sh
   for u in <Paks>/*.utoc; do retoc -a <key> list --path "$u"; done > files.txt
   ```

2. Decrypt the `Skill` table with `dectable.py`.
3. Add the icon names of new skills:

   ```sh
   python3 tools/icons/build_skill_icons.py Skill.bin files.txt assets/skills/skill_names.json assets/skills/skill_icon_tokens.json
   ```

   The script keeps the icon names that are in the file. It adds names for skills without an icon. To replace all names with the result of the script, add `--replace`. On 2026-10-15, the script read 24 names differently from the file. Nobody examined these 24 skills in the game.

4. Convert the skill textures (name part `ICON_`). Then write the icons that `skill_icon_tokens.json` names:

   ```sh
   python3 tools/icons/convert_textures.py <legacy dir> assets/skills/icons 48 assets/skills/skill_icon_tokens.json
   ```

On 2026-10-15, three icons were missing: `ICON_TE_SKILL_033`, `ICON_CH_SKILL_041` and `ICON_GT_SKILL_011`. Do these steps to add them.

### Item icons

1. Decrypt the `Item` table with `dectable.py`. Put `Item.bin` in a tables folder.
2. Make the icon name of each item:

   ```sh
   python3 tools/icons/build_item_map.py <tables dir> files.txt assets/items/item_info.json assets/items/item_icons.json
   ```

3. Convert the item textures (name part `Icon_`). Then write the icons at 64 px:

   ```sh
   python3 tools/icons/convert_textures.py <legacy dir> assets/items/icons 64 assets/items/item_icons.json
   ```

## Recordings

The scripts in `recording/` read recordings of game traffic. Use them to examine the game protocol. See [recording/README.md](recording/README.md).

## Map tiles

1. Dump the world map textures (`UI/Map/WorldMap/<map>/Res/<map>_<RR>_<CC>`) of `World_L_A`, `World_D_A` and `Abyss_Reshanta_A` into one folder.
2. Make the tiles. They are whole, as the game's own map shows them. Add `--mask` for the old transparent tiles, which keep only what looks like land:

   ```sh
   python3 tools/maps/build_maptiles.py <texture dir> <tile dir>
   ```

3. Export them for za2:

   ```sh
   python3 tools/maps/export_client_maps.py <tile dir> assets/maps tools/maps/calibration.json
   ```

   A whole tile gets no `_a.png` mask; the export deletes the old mask of such a tile.

`calibration.json` holds the conversion from world coordinates to map pixels for each map.

The game also has map textures for other maps, for example `World_L_B` and `World_D_B`. za2 does not show them yet. Each new map needs a calibration first.
