# za2 - zodiak for Aion 2

za2 is a DPS meter and interactive map for Aion 2 on Linux. It reads the game traffic and shows the damage, healing and skills of each player.

## Features

- See DPS, damage, healing, critical hits and skills for each player.
- See the combat power and gear score of the members of your party.
- Show all players, your group or only you.
- Use a map with your position, monsters, gathering spots and Hidden Cubes.
- Hear a sound that becomes faster near a Hidden Cube.
- See the level of each pet and the kills to the next level, and pin the pets you are farming to an overlay.
- Show the meter, the map and the pets as overlays on top of the game.

## Install

1. Download `za2-<version>-linux-x86_64.tar.gz` from the [releases](https://github.com/1ukah/za2/releases).
2. Extract the archive.
3. In the extracted folder, run:

   ```sh
   sudo ./install.sh
   ```

4. Start za2 from the application menu, or run `za2`.

To remove za2, run `sudo ./install.sh --uninstall`.

## Setup

- [Run from source](docs/run-from-source.md)
- [Usage](docs/usage.md)

## Important

za2 does not change the game or its traffic. The `za2-memory` helper reads the memory of the game to get your position and your pets. To use packets only, start za2 with `--no-memory`.

## Screenshots

<img width="1915" height="1047" alt="Screenshot_20261015_185529" src="https://github.com/user-attachments/assets/fc4ea51d-e654-4199-9faa-b0331170de9e" />

<img width="1916" height="1080" alt="Screenshot_20261015_185646" src="https://github.com/user-attachments/assets/12b51f2d-671b-486b-9d93-06230befe41d" />

<img width="1914" height="1046" alt="Screenshot_20261015_185809" src="https://github.com/user-attachments/assets/8756cb79-1b55-458c-a780-650f3e272ccf" />

## License

The code is MIT. See [LICENSE](LICENSE). The game data and artwork in `assets/` belong to their owners. The interface icons are from [Lucide](https://lucide.dev) (ISC license).

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-light.svg)](https://slint.dev)
