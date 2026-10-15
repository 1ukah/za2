# Recording tools

These Python scripts read recordings of game traffic. Use them to examine the game protocol. They read files only.

za2 writes recordings to `~/.local/share/za2/recordings/`. Each line of a recording is one TCP segment in JSON (`T`, `From`, `To`, `Seq`, `Server`, `Data` in Base64).

Run a script with `python3 -I <script> <file or folder>`.

| Script | Function |
|---|---|
| `frames.py` | Puts the stream together in the sequence of `Seq`. Cuts the varint frames. Unpacks the `FF FF` LZ4 bundles. Other scripts use its `iter_frames`. |
| `dump.py <file> <opcode hex> <count>` | Shows the first frames of an opcode, with the time and the length. |
| `p4738b_lib.py` | Reads the cooldown frame `0x4738` (`parse`). |
| `p0438c2.py <folder>` | Counts the frames `0438` without a damage block. It sorts them by whether a damage frame of the same skill is near (evade or resist). The file name pattern in the script is for the recordings of 2026-10-11 to 2026-10-13. Change it for other recordings. |

For a summary of each event, use `za2-replay <recording.jsonl> --verbose`. See [docs/usage.md](../../docs/usage.md).
