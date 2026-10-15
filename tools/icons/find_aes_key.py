"""Find the AES-256 key of the game's .pak files.

Unreal builds the key at run time in a small function that stores it as eight 32-bit immediates:
`mov dword [reg+0], imm32 ... mov dword [reg+28], imm32`. This scans for such runs (the AESDumpster approach) in
  - the game executable on disk:   find_aes_key.py --exe AION2.exe --pak pakchunk0-Windows_0_P.pak
  - the running game's memory:     find_aes_key.py --pid <pid> --pak pakchunk0-Windows_0_P.pak
and keeps a candidate only if it decrypts the index of the given .pak (its mount point reads "../").

Reading another process needs `kernel.yama.ptrace_scope=0` (or CAP_SYS_PTRACE); set it back to 1 afterwards.
Prints the key as `0x<64 hex>`, the form repak and retoc take. The key is not stored anywhere.
Try keyscan/ (see ../README.md) first: it reads the key itself from the game's memory and found it where this did not.
"""

import argparse
import re
import struct
import subprocess
import sys

PAK_MAGIC = struct.pack("<I", 0x5A6F12E1)


def aes256_ecb_decrypt(data: bytes, key: bytes) -> bytes:
    p = subprocess.run(["openssl", "enc", "-aes-256-ecb", "-d", "-K", key.hex(), "-nopad"],
                       input=data, capture_output=True, check=True)
    return p.stdout


def pak_index_block(path: str) -> bytes:
    """The first 16 encrypted bytes of the pak's index."""
    with open(path, "rb") as f:
        f.seek(0, 2)
        size = f.tell()
        f.seek(max(0, size - 4096))
        tail = f.read()
        at = tail.rfind(PAK_MAGIC)
        if at < 0:
            sys.exit(f"{path}: no pak footer")
        _version, offset, index_size = struct.unpack_from("<iqq", tail, at + 4)
        if not tail[at - 1]:
            sys.exit(f"{path}: the index is not encrypted")
        f.seek(offset)
        return f.read(16)


def opens_pak(key: bytes, block: bytes) -> bool:
    plain = aes256_ecb_decrypt(block, key)
    length = struct.unpack_from("<i", plain)[0]
    return 0 < length < 1024 and plain[4:7] == b"../"


def mov_imm32(data: bytes, p: int):
    """Decodes `mov dword [base+disp8], imm32` (C7 4x dd ii ii ii ii) or `mov dword [base], imm32` (C7 0x ...), with
    an optional REX.B prefix (41), at `p`. Returns (base register, displacement, imm32 bytes, end) or None. Bases that
    need a SIB byte (rsp, r12) or are rip-relative (rbp/r13 with mod 00) are not key stores."""
    if p < len(data) and data[p] == 0x41:
        p += 1
    if p + 6 > len(data) or data[p] != 0xC7:
        return None
    modrm = data[p + 1]
    mod, reg, base = modrm >> 6, (modrm >> 3) & 7, modrm & 7
    if reg != 0 or base == 4 or (mod == 0 and base == 5) or mod not in (0, 1):
        return None
    if mod == 0:
        return base, 0, data[p + 2:p + 6], p + 6
    if p + 7 > len(data):
        return None
    disp = data[p + 2] - (0x100 if data[p + 2] >= 0x80 else 0)
    return base, disp, data[p + 3:p + 7], p + 7


def candidates(data: bytes):
    """Keys stored by eight consecutive moves to one base register at displacements d, d+4, ..., d+28."""
    seen = set()
    for hit in re.finditer(rb"\xC7", data):
        p = hit.start()
        if p > 0 and data[p - 1] == 0x41:
            p -= 1
        stores = []
        for _ in range(8):
            m = mov_imm32(data, p)
            if not m or (stores and m[0] != stores[0][0]):
                break
            stores.append(m)
            p = m[3]
        if len(stores) < 8:
            continue
        first = stores[0][1]
        if [s[1] for s in stores] != [first + 4 * i for i in range(8)]:
            continue
        key = b"".join(s[2] for s in stores)
        if key not in seen and len(set(key)) > 12:
            seen.add(key)
            yield key


def regions(pid: int, executable: bool):
    """The readable memory of a process: the executable regions (where the unpacked code is) or all others."""
    with open(f"/proc/{pid}/maps") as maps, open(f"/proc/{pid}/mem", "rb", 0) as mem:
        for line in maps:
            fields = line.split()
            lo, hi = (int(x, 16) for x in fields[0].split("-"))
            if "r" not in fields[1] or ("x" in fields[1]) != executable or hi - lo > 512 << 20:
                continue
            try:
                mem.seek(lo)
                yield mem.read(hi - lo)
            except OSError:
                continue


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    source = ap.add_mutually_exclusive_group(required=True)
    source.add_argument("--exe")
    source.add_argument("--pid", type=int)
    ap.add_argument("--pak", required=True)
    args = ap.parse_args()

    block = pak_index_block(args.pak)
    if args.exe:
        chunks = [open(args.exe, "rb").read()]
    else:
        chunks = (data for executable in (True, False) for data in regions(args.pid, executable))
    tried = 0
    for data in chunks:
        for key in candidates(data):
            tried += 1
            if opens_pak(key, block):
                print(f"0x{key.hex().upper()}")
                return
    sys.exit(f"no key found ({tried} candidates tried)")


if __name__ == "__main__":
    main()
