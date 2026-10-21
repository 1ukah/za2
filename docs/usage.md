# Usage

## Parts

| Binary | Function |
|---|---|
| `za2` | The application: meter, map and overlays. It runs as a normal user and starts the helpers. |
| `za2-capture` | Finds the game connection and captures only its traffic. It needs `cap_net_raw,cap_net_admin`. |
| `za2-memory` | Reads your position, height, facing and map, the positions of near NPCs, and the level and progress of your pets, from the game memory. It needs `cap_sys_ptrace`. It is optional. |
| `za2-replay` | Replays and records game traffic on the command line. |

`za2` looks for the helpers in the same folder as itself.

## Options

| Option | Effect |
|---|---|
| `--replay FILE.jsonl [--speed N]` | Replay a recording. `--speed 0` replays as fast as possible. |
| `--x11` | Use XWayland. Then always-on-top for overlays works on KDE. |
| `--tab meter\|map\|timeline\|pets\|profile\|settings` | Start on this tab. |
| `--helper PATH` | Path of `za2-capture`. |
| `--memory-helper PATH` | Path of `za2-memory`. |
| `--no-memory` | Do not read the game memory. Positions and pets come from packets only. |
| `--assets DIR` | Folder of the game data. |
| `--send COMMAND` | Send a command to the running za2: `show`, `reset`, `toggle-overlay`, `toggle-map-overlay`, `toggle-pet-overlay`, `toggle-lock`, `map-zoom-in`, `map-zoom-out`. |
| `--version` | Show the version. |

## Pets

The Pets tab lists every pet with its level and the kills to the next level: 5 kills unlock a pet, 25 more give level 2, 75 more give level 3. Click a pet to pin it. The pet overlay shows the pinned pets, and the pet of the monster you fight. When you tick a pet or its monster in the monster list of the map, za2 pins the pet too, and unpins it when you untick it (Settings > Overlays turns this off).

The numbers come from the game memory when `za2-memory` runs. Without it, they come from the game traffic: the full list arrives only when you log in, so start za2 before you log in. Then each kill is counted. za2 keeps the last numbers of each character for the next start.

## Profile

The Profile tab shows what za2 knows of your character: name, class, level, faction, server, legion, Odyle energy, and your pets. Combat power and gear score come from the party list, so za2 knows them only while you are in a party. The ping is in the tooltip of the connection dot at the bottom left.

## DPS

za2 opens on the Map. The DPS tab has two sub tabs at the top: Meter and Timeline. On the Timeline, click a name above the chart or a line in the chart to see the effects on that player, or with "On target" the effects on what they hit most. Saved boss fights keep both.

In the Pets tab, click a column title (Pet, Level, Progress, Left) to sort by it; click it again for the other order. Pinned pets stay at the top.

## Meter: Fight and Total

`Fight` shows the current fight. A fight ends after 10 seconds without combat; the next hit starts a new one. `Total` adds up every fight since you entered the zone, or since the last reset. Until the second fight starts, both show the same numbers. With "Reset on zone change" on, a teleport or a zone change starts the total again.

## Shortcuts

| Key | Effect |
|---|---|
| `Ctrl+R` | Reset the meter. |
| `Ctrl+O` | Show or hide the meter overlay. |
| `Ctrl+M` | Show or hide the map overlay. |
| `Ctrl+P` | Show or hide the pet overlay. |
| `Ctrl+L` | Lock or unlock the overlays. |
| `1`, `2`, `3`, `4` | Open the Map, DPS (Meter and Timeline), Pets or Profile tab. |

The overlays start unlocked at every start, so you can move them. Lock them with `Ctrl+L` or their lock button.

These keys work only when the za2 window has focus. For a global shortcut on Wayland, bind a `za2 --send` command in your desktop. On KDE Plasma: System Settings > Keyboard > Shortcuts > Add Command, for example `za2 --send toggle-overlay`.

## Files

| File | Content |
|---|---|
| `~/.config/za2/settings.json` | Settings. |
| `~/.local/share/za2/sessions.json` | Sessions, one for each zone stay. za2 keeps them for 7 days. |
| `~/.local/share/za2/pets.json` | The level and progress of the pets of each character, from the last session. |
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
