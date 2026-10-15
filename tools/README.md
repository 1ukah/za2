# Data tools

These Python scripts make the game data in `assets/` from the files of an installed game client. za2 does not need them to build or run. Use them after a game update.

The scripts read local files only.

## Requirements

- Python 3 with `Pillow` and `cryptography`:

  ```sh
  python3 -m venv .venv
  .venv/bin/pip install Pillow cryptography
  ```

- [repak](https://github.com/trumank/repak) to unpack the game `.pak` files:

  ```sh
  cargo install --git https://github.com/trumank/repak repak_cli
  ```

- The AES key of the game `.pak` files. The key is not in this repository.
- `tools/dat/constants.local.json` with the two constants of the `.dat` format. They are in the public [CUE4Parse](https://github.com/FabianFG/CUE4Parse) sources (`FAion2KeyManifestFile._manifestHashKey`, `Aion2DatFileEncryption.HeaderXorConstant`). Git ignores this file.

  ```json
  { "manifest_hash_key_hex": "<hex>", "header_xor_constant": "0x<hex>" }
  ```

## Steps

1. Unpack the files from the game `Paks` folder:

   ```sh
   repak --aes-key 0x<key> unpack -i "Aion2/Content/Data/Map/**" -i "Aion2/Content/Data/Table/*.dat" -o <out> pakchunk401000-Windows_0_P.pak
   repak --aes-key 0x<key> unpack -i "Aion2/Content/StartUpData/Table/key_manifest.dat" -o <out> pakchunk0-Windows.pak
   ```

   Also unpack `L10N/Text/<locale>/L10NString.dat` from each pak in `Paks/L10N/Text/`. The scripts expect the folder name `AION2/`. A symbolic link from `AION2` to `Aion2` is sufficient.

2. Decrypt the tables and the texts:

   ```sh
   python3 tools/dat/dectable.py <out>/Aion2/Content/StartUpData/Table/key_manifest.dat <table>.dat <table>.bin
   python3 tools/dat/extract_l10n.py <out> <l10n dir>
   ```

   Do `dectable.py` for `NpcData`, `VehicleList`, `EnvObjData`, `WorldMapFieldNamed` and `Item`.

3. Make the data files:

   | Script | Output |
   |---|---|
   | `dat/build_npcs.py <NpcData.bin> <l10n dir> <out.json>` | `assets/npcs/npc_names.json` |
   | `dat/build_pets.py <VehicleList.bin> <NpcData.bin> <npc_names.json> <l10n dir> <out.json> dat/extra_pets.json [Item.bin]` | `assets/pets/pets.json` |
   | `dat/build_spawns.py <out>/AION2/Content/Data/Map <pets.json> <out.json>` | `assets/pets/spawns.json` |
   | `dat/build_gather.py <out>/AION2/Content/Data/Map <EnvObjData.bin> <l10n dir> <out.json>` | `assets/maps/gather.json` |
   | `dat/build_field_bosses.py <NpcData.bin> <WorldMapFieldNamed.bin> <npc_names.json> <l10n dir> <out>/AION2/Content/Data/Map <out.json>` | `assets/bosses/field_bosses.json` |

## Map tiles

1. Dump the world map textures (`UI/Map/WorldMap/<map>/Res/<map>_<RR>_<CC>`) of `World_L_A`, `World_D_A` and `Abyss_Reshanta_A` into one folder.
2. Make the transparent tiles:

   ```sh
   python3 tools/maps/build_maptiles.py <texture dir> <tile dir>
   ```

3. Export them for za2:

   ```sh
   python3 tools/maps/export_client_maps.py <tile dir> assets/maps tools/maps/calibration.json
   ```

`calibration.json` holds the conversion from world coordinates to map pixels for each map.
