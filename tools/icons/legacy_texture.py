"""Decodes a UI texture that `retoc to-legacy` wrote (`<name>.uasset` + `<name>.uexp`) into a PIL image.

    python3 legacy_texture.py <name.uexp> <out.png>

A UI texture holds one mip inline. Each texture entry is its pixel format name (`PF_...`), with the size as three i32
(width, height, depth) 16 bytes before it and the pixels 29 bytes after the end of it. A sprite atlas has one entry per page.
"""
import io
import os
import re
import struct
import sys

from PIL import Image

DXGI = {"PF_BC4": 80, "PF_BC5": 83, "PF_BC7": 98}


def dds(width, height, fourcc, data, dxgi=None):
    header = struct.pack("<4sIIIIIII44sII4sIIIII", b"DDS ", 124, 0x81007, height, width, len(data), 0, 1, b"\0" * 44, 32, 4,
                         b"DX10" if dxgi else fourcc, 0, 0, 0, 0, 0)
    header += struct.pack("<IIIII", 0x1000, 0, 0, 0, 0)
    if dxgi:
        header += struct.pack("<IIIII", dxgi, 3, 0, 1, 0)
    return header + data


def _size(fmt, width, height):
    blocks = max(1, width // 4) * max(1, height // 4)
    return {"PF_DXT1": blocks * 8, "PF_BC4": blocks * 8, "PF_DXT5": blocks * 16, "PF_BC5": blocks * 16, "PF_BC7": blocks * 16,
            "PF_B8G8R8A8": width * height * 4, "PF_G8": width * height}.get(fmt)


def _image(fmt, width, height, data):
    if fmt == "PF_B8G8R8A8":
        return Image.frombytes("RGBA", (width, height), data, "raw", "BGRA")
    if fmt == "PF_G8":
        return Image.frombytes("L", (width, height), data).convert("RGBA")
    if fmt in DXGI:
        return Image.open(io.BytesIO(dds(width, height, None, data, DXGI[fmt]))).convert("RGBA")
    return Image.open(io.BytesIO(dds(width, height, fmt[3:].encode(), data))).convert("RGBA")


def pages(path):
    """Every texture in the file: one for a texture, one per page for a sprite atlas. The pixels of an entry start 29 bytes
    after the end of its pixel format name (the name's null, mip flags and the bulk data header), or a larger texture's are
    in the .ubulk next to the file."""
    d = open(path, "rb").read()
    bulk_path = path[:-5] + ".ubulk"
    out = []
    for m in re.finditer(rb"PF_[A-Za-z0-9_]+", d):
        fmt = m.group().decode()
        width, height, _ = struct.unpack_from("<iii", d, m.start() - 16)
        size = _size(fmt, width, height)
        if not size or not (0 < width <= 8192 and 0 < height <= 8192):
            continue
        start = m.end() + 29
        if start + size <= len(d):
            out.append(_image(fmt, width, height, d[start:start + size]))
        elif os.path.exists(bulk_path):
            # larger textures keep their mips in the .ubulk, the biggest first
            bulk = open(bulk_path, "rb").read()
            if len(bulk) >= size:
                out.append(_image(fmt, width, height, bulk[:size]))
    return out


def decode(path):
    """The texture of a texture file (the last entry when the data is there twice)."""
    found = pages(path)
    return found[-1] if found else None


def sprite_rect(uexp_path, atlas_size):
    """Pixel rectangle (left, top, right, bottom) of a PaperSprite in its atlas page, from the baked render data: vertices of
    four doubles (x, y, u, v) with u and v in 0..1."""
    d = open(uexp_path, "rb").read()
    us, vs = [], []
    for at in range(0, len(d) - 31):
        x, y, u, v = struct.unpack_from("<dddd", d, at)
        if 0.0 <= u <= 1.0 and 0.0 <= v <= 1.0 and x == x and y == y and x and y and x.is_integer() and y.is_integer() \
                and abs(x) < 4096 and abs(y) < 4096:
            us.append(u)
            vs.append(v)
    if not us:
        return None
    w, h = atlas_size
    return round(min(us) * w), round(min(vs) * h), round(max(us) * w), round(max(vs) * h)


if __name__ == "__main__":
    image = decode(sys.argv[1])
    if image is None:
        sys.exit(f"{sys.argv[1]}: not a texture this can decode")
    image.save(sys.argv[2])
