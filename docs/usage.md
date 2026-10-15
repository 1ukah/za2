# Usage

## Parts

| Binary | Function |
|---|---|
| `za2` | The application: meter, map and overlays. It runs as a normal user and starts the helpers. |
| `za2-capture` | Finds the game connection and captures only its traffic. It needs `cap_net_raw,cap_net_admin`. |
| `za2-memory` | Reads your position, height, facing and map, and the positions of near NPCs, from the game memory. It needs `cap_sys_ptrace`. It is optional. |
| `za2-replay` | Replays and records game traffic on the command line. |

`za2` looks for the helpers in the same folder as itself.

## Options

| Option | Effect |
|---|---|
| `--replay FILE.jsonl [--speed N]` | Replay a recording. `--speed 0` replays as fast as possible. |
| `--x11` | Use XWayland. Then always-on-top for overlays works on KDE. |
| `--tab meter\|map\|settings` | Start on this tab. |
| `--helper PATH` | Path of `za2-capture`. |
| `--memory-helper PATH` | Path of `za2-memory`. |
| `--no-memory` | Do not read the game memory. Positions come from packets only. |
| `--assets DIR` | Folder of the game data. |
| `--send COMMAND` | Send a command to the running za2: `show`, `reset`, `toggle-overlay`, `toggle-map-overlay`, `toggle-lock`, `map-zoom-in`, `map-zoom-out`. |
| `--version` | Show the version. |

## Shortcuts

| Key | Effect |
|---|---|
| `Ctrl+R` | Reset the meter. |
| `Ctrl+O` | Show or hide the meter overlay. |
| `Ctrl+M` | Show or hide the map overlay. |
| `Ctrl+L` | Lock or unlock the overlays. |
| `1`, `2`, `3` | Open the Meter, Map or Settings tab. |

These keys work only when the za2 window has focus. For a global shortcut on Wayland, bind a `za2 --send` command in your desktop. On KDE Plasma: System Settings > Keyboard > Shortcuts > Add Command, for example `za2 --send toggle-overlay`.

## Files

| File | Content |
|---|---|
| `~/.config/za2/settings.json` | Settings. |
| `~/.local/share/za2/sessions.json` | Sessions, one for each zone stay. za2 keeps them for 7 days. |
| `~/.local/share/za2/recordings/` | Recordings of game traffic. |

## Overlays on KDE Plasma

On Wayland, KWin does not let an application keep its windows on top. A fullscreen game is above "keep above" windows. Add a window rule to put the overlays above the game:

1. Open System Settings > Window Management > Window Rules.
2. Add a rule with window class `za2` and window title that contains `overlay`.
3. Set Layer to Overlay (Force).
4. Set Keep above to Yes (Force).
5. Optional: set skip taskbar, skip switcher and skip pager.

When the overlays are locked, mouse clicks go through them to the game. On X11 and XWayland (`--x11`), always-on-top and click-through work without a rule.

## Command line

Replay a recording and show each event:

```sh
za2-replay recording.jsonl --verbose
```

Record 120 seconds of game traffic:

```sh
za2-capture --ports 13328 | za2-replay --from-capture --record out.jsonl --seconds 120
```
