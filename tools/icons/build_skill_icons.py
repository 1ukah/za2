"""Skill icon names from the client's Skill.dat: the row's icon path ('Skill/ICON_xx_SKILL_nnn', plain text) follows the row's
STR_SKILL_PC_<CLASS>_<id> string, before the next row's string.

Usage: build_skill_icons.py <Skill.bin from dectable.py> <files.txt> <assets/skills/skill_names.json> <assets/skills/skill_icon_tokens.json> [--replace]

<files.txt> lists the IoStore files (`retoc -a <key> list --path <utoc>` of all containers): only icons that exist are used.
Writes {first four digits of the skill id: icon name}, the form za2 reads. By default the entries of the old file stay as they
are and only skills without an icon get one: on 2026-10-15 this read 24 of the 953 entries differently (mostly neighbouring
skills of one class), which were not checked in game. --replace lets this run's result win.
"""
import bisect, json, os, re, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dat  # noqa: E402
skill_bin, files, names_path, out = sys.argv[1:5]
replace = "--replace" in sys.argv[5:]
d = open(skill_bin, 'rb').read()
ss = dat.strings(d)
tex = {l.strip().rsplit('/', 1)[-1][:-7].lower(): l.strip().rsplit('/', 1)[-1][:-7] for l in open(files) if 'Resource/Texture/Skill/' in l and l.strip().endswith('.uasset')}
icons = [(m.start(), m.group(1).decode()) for m in re.finditer(rb'Skill/(ICON_[A-Za-z0-9_]+)\x00', d)]
strs = sorted((o, int(t.rsplit('_', 1)[1])) for o, t, e in ss if re.match(r'STR_SKILL_PC_[A-Z]+_\d+$', t))
offs = [s[0] for s in strs]
CODE = {11: 'GL', 12: 'TE', 13: 'AS', 14: 'RA', 15: 'SO', 16: 'EL', 17: 'CL', 18: 'CH', 19: 'CO'}
# icons in the row's own span (its STR string to the next row's), of the skill's own class only
cands = {}
for off, ic in icons:
    i = bisect.bisect_right(offs, off) - 1
    if i < 0 or off - strs[i][0] > 1200 or ic.lower() not in tex:
        continue
    sid = strs[i][1]
    cands.setdefault(sid, []).append(tex[ic.lower()])
sole = {}
for sid, l in cands.items():
    if len(set(l)) == 1:
        sole.setdefault(l[0], set()).add(sid)
names = json.load(open(names_path))
old = {}
try:
    old = json.load(open(out))
except Exception:
    pass
res = {}
for k in names:
    sid = int(k)
    if sid % 10000:
        continue
    pool = cands.get(sid) or next((cands[v] for v in range(sid + 10, sid + 400, 10) if v in cands), [])
    uniq = list(dict.fromkeys(pool))
    # the skill's own class set first; some skills use another class's art (Sword Aura Rampage: ICON_TE_SKILL_034)
    own = [u for u in uniq if u.split('_')[1] == CODE.get(sid // 1000000)]
    uniq = own or uniq
    if len(uniq) > 1:   # two icons in one row: the other one belongs to a neighbouring skill
        free = [u for u in uniq if not (sole.get(u, set()) - {sid})]
        uniq = free or uniq
    if uniq:
        res[str(sid // 10000)] = uniq[0]
merged = {**old, **res} if replace else {**res, **old}
json.dump(dict(sorted(merged.items())), open(out, 'w'), indent=2)
print(len(res), 'skill families found,', len(merged), 'in', out)
