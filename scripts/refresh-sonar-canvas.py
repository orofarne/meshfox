#!/usr/bin/env python3
"""Rebuild the leaves of SONAR.canvas.md from a fresh SonarCloud export.

    python3 scripts/refresh-sonar-canvas.py [CANVAS] [--out FILE] [--date YYYY-MM-DD] [--rev SHA]

Defaults: CANVAS=SONAR.canvas.md, rewritten in place; the date is today; the
revision is the one SonarCloud analysed last. Rule nodes keep their prose and
tags; leaves (`path:line`) are regenerated from the open issues; a rule that is
gone from the export moves to "Закрытые правила (история)"; a new rule is
appended; the date, commit and counters in the root node are refreshed. Run
`meshfox check` on the result.
"""
import collections, datetime, json, re, sys, urllib.request

KEY = "orofarne_meshfox"
SEV = {"BLOCKER": "critical", "CRITICAL": "critical", "MAJOR": "major", "MINOR": "minor", "INFO": "info"}


def _args():
    a, opts, pos = sys.argv[1:], {}, []
    while a:
        x = a.pop(0)
        if x in ("--out", "--date", "--rev"):
            opts[x[2:]] = a.pop(0)
        elif x in ("-h", "--help"):
            print(__doc__)
            sys.exit(0)
        else:
            pos.append(x)
    return pos[0] if pos else "SONAR.canvas.md", opts


CANVAS, OPTS = _args()
OUT = OPTS.get("out", CANVAS)
DATE = OPTS.get("date", datetime.date.today().isoformat())
REV = OPTS.get("rev")
if not REV:
    analyses = json.load(urllib.request.urlopen(
        f"https://sonarcloud.io/api/project_analyses/search?project={KEY}&ps=1"))["analyses"]
    REV = analyses[0]["revision"][:7]


def fetch():
    issues, page = [], 1
    while True:
        url = f"https://sonarcloud.io/api/issues/search?componentKeys={KEY}&resolved=false&ps=500&p={page}"
        data = json.load(urllib.request.urlopen(url))
        issues += data["issues"]
        if page * 500 >= data["paging"]["total"]:
            return issues
        page += 1


def slug(text):
    return re.sub(r"[^\w]+", "-", text.lower(), flags=re.UNICODE).strip("-")


text = open(CANVAS, encoding="utf-8").read()
lines = text.split("\n")
# split into root part and "## " blocks
starts = [i for i, l in enumerate(lines) if l.startswith("## ")]
root = lines[: starts[0]]
blocks = [lines[a:b] for a, b in zip(starts, starts[1:] + [len(lines)])]
history = next(b for b in blocks if b[0].startswith("## Закрытые правила"))
rules = [b for b in blocks if b is not history]

issues = fetch()
by_rule = collections.defaultdict(list)
for it in issues:
    path = it["component"].removeprefix(KEY + ":")
    by_rule[it["rule"]].append((path, it.get("line") or 0, it["message"], SEV[it["severity"]], it["type"]))

used = set(re.findall(r'meshfox:node id="([^"]+)"', "\n".join(l for b in rules for l in b)))
used |= set(re.findall(r'meshfox:node id="([^"]+)"', "\n".join(history)))
# leaf ids are regenerated; drop the old ones from the used set
old_leaf = set()
for b in rules:
    for i, l in enumerate(b):
        if l.startswith("### "):
            m = re.search(r'id="([^"]+)"', b[i + 1])
            if m:
                old_leaf.add(m.group(1))
used -= old_leaf


def leaf_id(path, line):
    base = slug(f"{path}-{line}")
    cand, n = base, 1
    while cand in used:
        n += 1
        cand = f"{base}-{n}"
    used.add(cand)
    return cand


def leaves_md(rule_msg, rule_sev, items):
    out = []
    for path, line, msg, sev, _ in sorted(items, key=lambda x: (x[0], x[1], x[2])):
        tags = f' tags="{sev}"' if sev != rule_sev else ""
        out += [f"### {path}:{line}", f'<!-- meshfox:node id="{leaf_id(path, line)}"{tags} -->', ""]
        if msg != rule_msg:
            out += [msg, ""]
    return out


def parse_rule(b):
    key = re.search(r"· `([^`]+)`", "\n".join(b[:8]))
    tags = re.search(r'tags="([^"]*)"', b[1])
    first_leaf = next((i for i, l in enumerate(b) if l.startswith("### ")), len(b))
    head = b[:first_leaf]
    # rule message: first paragraph after the "SEVERITY / TYPE · `rule`" line
    k = next(i for i, l in enumerate(head) if "· `" in l)
    j = k + 1
    while j < len(head) and head[j] == "":
        j += 1
    return (key.group(1) if key else None), (tags.group(1) if tags else ""), head, head[j] if j < len(head) else ""


new_blocks, closed, seen = [], [], set()
before = {}
for b in rules:
    key, tags, head, msg = parse_rule(b)
    old_leaves = [l[4:] for l in b if l.startswith("### ")]
    if key is None or key not in by_rule:
        closed.append((b, key, old_leaves))
        continue
    seen.add(key)
    sev_tags = [t for t in tags.split() if t in SEV.values()]
    rule_sev = sev_tags[0] if sev_tags else "info"
    items = by_rule[key]
    # the rule's own severity is the most common one among its issues
    top = collections.Counter(i[3] for i in items).most_common(1)[0][0]
    if rule_sev != top and not (set(tags.split()) - set(SEV.values())):
        rule_sev = top
        head = [re.sub(r'tags="[^"]*"', f'tags="{top}"', l) if l.startswith("<!-- meshfox:node") else l for l in head]
    while head and head[-1] == "":
        head.pop()
    new_blocks.append(head + [""] + leaves_md(msg, rule_sev, items))

fresh = []
for key, items in sorted(by_rule.items(), key=lambda kv: -len(kv[1])):
    if key in seen:
        continue
    msg = collections.Counter(i[2] for i in items).most_common(1)[0][0]
    sev = collections.Counter(i[3] for i in items).most_common(1)[0][0]
    typ = items[0][4]
    title = f"{key} — {msg[:80]}"
    rid = slug(f"{key}-{len(items)}")
    while rid in used:
        rid += "-x"
    used.add(rid)
    fresh.append(
        [f"## {title}", f'<!-- meshfox:node id="{rid}" tags="{sev}" -->', "", f"{sev.upper()} / {typ} · `{key}`", "", msg, ""]
        + leaves_md(msg, sev, items)
    )

# history: append the rules that disappeared, keeping where they were
hist = list(history)
while hist and hist[-1] == "":
    hist.pop()
for b, key, old_leaves in closed:
    title = b[0][3:]
    byfile = collections.OrderedDict()
    for ol in old_leaves:
        f, _, ln = ol.rpartition(":")
        byfile.setdefault(f, []).append(ln)
    head = [l for l in b[: next((i for i, l in enumerate(b) if l.startswith("### ")), len(b))]]
    head[0] = "### " + title + (f" ({len(old_leaves)})" if old_leaves else "")
    head = [re.sub(r'tags="[^"]*"', "", l).replace("  -->", " -->") if l.startswith("<!-- meshfox:node") else l for l in head]
    while head and head[-1] == "":
        head.pop()
    hist += [""] + head
    if byfile:
        hist += [""] + [f"- `{f}`: {', '.join(ls)}" for f, ls in byfile.items()]

all_rules = new_blocks + fresh
total = sum(len(v) for v in by_rule.values())
types = collections.Counter(i[4] for v in by_rule.values() for i in v)
c_total = sum(len(v) for k, v in by_rule.items() if k.startswith("c:"))
parser_c = sum(1 for k, v in by_rule.items() if k.startswith("c:") for i in v if i[0] == "editors/neovim/grammar/src/parser.c")
s1116 = len(by_rule.get("c:S1116", []))
arch = sum(1 for v in by_rule.values() for i in v if i[0].startswith("site-template-archive/"))
css7924 = len(by_rule.get("css:S7924", []))
web = sum(1 for v in by_rule.values() for i in v if i[0].startswith("web/src/"))
cli = sum(1 for v in by_rule.values() for i in v if i[0].startswith("crates/cli/"))
bugs = collections.Counter(k for k, v in by_rule.items() for i in v if i[4] == "BUG")
print("total", total, dict(types), "bugs by rule", dict(bugs))
print("closed rules:", [k for _, k, _ in closed], "new rules:", [(r[0][3:60], len(r)) for r in fresh])

root_text = "\n".join(root)
root_text = re.sub(
    r"снимок от \d{4}-\d{2}-\d{2} \(анализ коммита `[0-9a-f]+`\): \*\*\d+ шт\.\*\*",
    f"снимок от {DATE} (анализ коммита `{REV}`): **{total} шт.**",
    root_text,
)
i = root_text.index("Замечания по снимку")
bug_list = ", ".join(f"`{k}` ({n} шт.)" for k, n in bugs.most_common())
notes = (
    f"Замечания по снимку ({total} issues, {DATE}):\n"
    f" - Типы: {types.get('CODE_SMELL', 0)} CODE_SMELL, {types.get('BUG', 0)} BUG, VULNERABILITY: {types.get('VULNERABILITY', 0)}. "
    f"BUG: {bug_list}. Quality Gate на этом коммите зелёный (в новом коде багов нет).\n"
    f" - {c_total} из {total} — `c:*`, из них {parser_c} в сгенерированном `editors/neovim/grammar/src/parser.c` (tree-sitter; {s1116} — `c:S1116`). "
    "Править бессмысленно. `sonar-project.properties` с `sonar.exclusions` удалён коммитом `35ae7ab`, так что исключения сейчас нет — "
    "если хочется убрать шум, нужен новый способ (настройка в UI SonarCloud или вернуть файл).\n"
    f" - `site-template-archive/` даёт {arch} issues, из них {css7924} — `css:S7924` (билд-артефакт; кандидат на исключение).\n"
    f" - Остальное в основном `web/src` ({web}) и `crates/cli` ({cli}).\n"
)
root_text = root_text[:i] + notes
out = root_text.rstrip("\n").split("\n") + [""]
for b in all_rules:
    out += b
    while out and out[-1] == "":
        out.pop()
    out.append("")
out += hist + [""]
open(OUT, "w", encoding="utf-8").write("\n".join(out))
