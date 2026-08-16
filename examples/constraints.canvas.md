<!-- meshfox:canvas -->
# Constraints Demo
<!-- meshfox:node id="root" x=0 y=0 w=280 h=60 -->

Every ` ```starlark constraint ` capability, side by side — see SPEC.md's
"Constraint fences" section for the full reference. Run `meshfox check
examples/constraints.canvas.md` from the repo root to see every one of
these actually evaluate (they're all meant to pass).

## Structural check
<!-- meshfox:node id="structural-check" -->

A constraint that only ever reads the document tree itself
(`.descendants()`/`.tags`/`.children()`) — no file I/O, straight from
SPEC.md's own example: every node tagged `table` below must have exactly
one `file` child.

```starlark constraint
for n in self.descendants():
    if "table" in n.tags:
        files = [c for c in n.children() if c.type == "file"]
        if len(files) != 1:
            fail(n.id + ": expected exactly one file child, got " + str(len(files)))
```

### Users
<!-- meshfox:node id="users" tags="table" -->

#### Schema
<!-- meshfox:node id="schema" type="file" -->

[users-schema.sql](constraints-data/users-schema.sql)

### Other Table
<!-- meshfox:node id="other-table" tags="table" -->

#### Schema
<!-- meshfox:node id="schema-2" type="file" -->

[other-schema.sql](constraints-data/other-schema.sql)

## Reading files
<!-- meshfox:node id="reading-files" -->

A `file`-type node's own already-declared target, read fresh off disk and
confined to the canvas's own directory — `.content()` for the raw text,
`.json()`/`.yaml()`/`.toml()` for that same content parsed into a
Starlark dict, `.csv()` for tabular data as a list of dicts keyed by
header. All five return `None` for anything that isn't a `file` node, has
no target, or doesn't parse that way — a constraint decides for itself
whether that's `fail`-worthy.

```starlark constraint
# Looked up by title, not id: a literal id (`self.node("notes")`) stops
# resolving once this file is included elsewhere and its ids get
# namespaced `{include-id}/{original-id}` (see SPEC.md's "Includes") --
# `self.children()` plus a title match survives that splicing.
def _file_child(title):
    for c in self.children():
        if c.type == "file" and c.title == title:
            return c
    return None

if "prose, read as-is" not in _file_child("Notes").content():
    fail("notes.txt: unexpected content")

info = _file_child("Info (JSON)").json()
if info["project"] != "meshfox" or info["stable"] != True:
    fail("info.json: unexpected data: " + str(info))

info = _file_child("Info (YAML)").yaml()
if info["project"] != "meshfox" or info["stable"] != True:
    fail("info.yaml: unexpected data: " + str(info))

rows = _file_child("Rows (CSV)").csv()
if len(rows) != 2 or rows[0]["name"] != "anyhow" or rows[1]["name"] != "serde":
    fail("rows.csv: unexpected rows: " + str(rows))

# A node with no target at all -- self, right here -- exposes none of this.
if self.content() != None or self.json() != None:
    fail("a plain node should have no file data of its own")
```

### Notes
<!-- meshfox:node id="notes" type="file" -->

[notes.txt](constraints-data/notes.txt)

### Info (JSON)
<!-- meshfox:node id="info-json" type="file" -->

[info.json](constraints-data/info.json)

### Info (YAML)
<!-- meshfox:node id="info-yaml" type="file" -->

[info.yaml](constraints-data/info.yaml)

### Rows (CSV)
<!-- meshfox:node id="rows-csv" type="file" -->

[rows.csv](constraints-data/rows.csv)

## Dependency audit
<!-- meshfox:node id="dependency-audit" -->

The pattern this whole feature exists for — and exactly what
`LICENSE.canvas.md`'s own `backend-deps`/`ui-deps` nodes use for real:
`.toml()` a manifest, extract the crate names documented in this node's
own table (from `self.text`, since a constraint can't see a Markdown
table any other way), and `fail` for anything in the manifest with no row
here. `meshfox-core` is skipped — it's a workspace-internal path
dependency, not a third-party one.

| Crate | License |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| serde | MIT OR Apache-2.0 |

```starlark constraint
def _table_names(text):
    names = []
    for raw_line in text.split("\n"):
        line = raw_line.strip()
        if not line.startswith("|"):
            continue
        cells = line.split("|")
        if len(cells) < 3:
            continue
        name = cells[1].strip()
        if name == "" or name == "Crate" or name.startswith("-"):
            continue
        names.append(name)
    return names

manifest_nodes = [c for c in self.children() if c.type == "file"]
manifest = manifest_nodes[0].toml() if len(manifest_nodes) == 1 else None
documented = _table_names(self.text)
if manifest == None:
    fail("manifest.toml: could not find/read/parse it")
else:
    for name in manifest["dependencies"]:
        v = manifest["dependencies"][name]
        if type(v) == "dict" and "path" in v:
            continue  # workspace-internal crate, not third-party
        if name not in documented:
            fail(name + " is a direct dependency but has no entry in the table above")
```

### manifest.toml
<!-- meshfox:node id="manifest-toml" type="file" -->

[manifest.toml](constraints-data/manifest.toml)

