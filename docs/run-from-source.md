# Run from source

## Requirements

- Linux, x86_64.
- Rust (stable). Install it with [rustup](https://rustup.rs).
- PipeWire (`pw-play`) or PulseAudio (`paplay`) for the Hidden Cube sound.
- The packages for your distribution:

| Distribution | Command |
|---|---|
| Arch, CachyOS | `sudo pacman -S --needed base-devel git pkgconf libpcap fontconfig freetype2 libcap` |
| Debian, Ubuntu | `sudo apt install build-essential git pkg-config libpcap-dev libfontconfig1-dev libfreetype-dev libcap2-bin` |
| Fedora | `sudo dnf install gcc git pkgconf-pkg-config libpcap-devel fontconfig-devel freetype-devel libcap` |

## Build

```sh
git clone https://github.com/1ukah/za2.git
cd za2
cargo build --release
```

The binaries are in `target/release/`.

## Give the helpers their rights

`za2-capture` reads the game traffic. `za2-memory` reads your position from the game memory. Run these commands:

```sh
sudo setcap cap_net_raw,cap_net_admin=eip target/release/za2-capture
sudo setcap cap_sys_ptrace=eip target/release/za2-memory
```

Run them again after each build. If you do not give `za2-memory` its right, za2 gets your position from the packets only.

## Run

```sh
target/release/za2
```

za2 finds the game data in `assets/` of the project folder. For the options, see [Usage](usage.md).

## Install from source

To install your build system-wide, with an application menu entry, run:

```sh
sudo ./install.sh
```

## Test

```sh
cargo test
```

## Make a release

1. Change the version in `VERSION`, for example `1.0.1`.
2. Commit the change.
3. Run:

   ```sh
   git tag v1.0.1
   git push origin master v1.0.1
   ```

The GitHub Action builds `za2-1.0.1-linux-x86_64.tar.gz` and adds it to a new release. The tag must be the same as `VERSION`, and the commit must be on `master`.

## Update the game data

The data in `assets/` comes from the game files. After a game update, you can make it again with the scripts in `tools/`. See [tools/README.md](../tools/README.md).
