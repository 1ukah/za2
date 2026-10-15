"""Writes converted game textures as square PNG files.

    python3 convert_textures.py <legacy dir> <out dir> <size> [names.json]

<legacy dir> holds textures that `retoc to-legacy` wrote. Each texture becomes <out dir>/<texture name>.png, <size> x <size> px.
With [names.json] (a JSON object whose values are texture names, as skill_icon_tokens.json and item_icons.json are), only those
textures are written. Textures this cannot decode are listed at the end.
"""
import json
import os
import sys

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import legacy_texture  # noqa: E402


def main():
    legacy, out, size = sys.argv[1], sys.argv[2], int(sys.argv[3])
    wanted = None
    if len(sys.argv) > 4:
        wanted = {str(v).rsplit("/", 1)[-1] for v in json.load(open(sys.argv[4])).values()}
    os.makedirs(out, exist_ok=True)
    written, failed = 0, []
    for folder, _, files in os.walk(legacy):
        for f in sorted(files):
            name = f[:-5]
            if not f.endswith(".uexp") or (wanted is not None and name not in wanted):
                continue
            image = legacy_texture.decode(os.path.join(folder, f))
            if image is None:
                failed.append(name)
                continue
            image.resize((size, size), Image.LANCZOS).save(os.path.join(out, name + ".png"), optimize=True)
            written += 1
    print(f"{written} textures -> {out}")
    if failed:
        print("not decoded:", " ".join(failed))


if __name__ == "__main__":
    main()
