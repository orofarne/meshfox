<!-- meshfox:canvas -->
# SVG in Markdown

Showing an SVG (a diagram, a formula, a chart) inside a node's Markdown, the same way in the web UI, `meshfox tui`, `meshfox static` and `meshfox pdf` — with no renderer built into meshfox: any command that can print an SVG will do (Graphviz, Typst, Mermaid, PlantUML, Vega, matplotlib, ...).

An SVG reaches the page as an ordinary Markdown image whose URL is a `data:image/svg+xml;base64,…` URI, so nothing has to sit next to the canvas. There are two ways to produce it, both on a `cache`d block so the picture is saved in the file under the fence:

- `output="markdown"` — the block's stdout *is* Markdown; the script prints the `![alt](data:…)` line itself (as in [pandas-dataframe.canvas.md](./pandas-dataframe.canvas.md)). Full control, a little boilerplate. See **output=markdown** below.
- `output="image"` — the block's stdout *is the SVG*; meshfox wraps it into that image line. One-liners like `plantuml -tsvg -pipe` just work. See **output=image** below.

Two more things are covered further down: how an SVG follows the light/dark **theme**, and the `bg=` **background** for diagrams drawn for a white page.

Every block below is self-contained (it prints a small hand-written SVG), so this file runs without any extra tools — except the **Real renderers** node, whose blocks need the named tool installed (the Python one creates its own virtualenv on first run).

## output=markdown
<!-- meshfox:node id="output-markdown" -->

The script prints the whole Markdown image itself. `base64 | tr -d '\n'` is the portable spelling (GNU `base64` wraps lines at 76 columns, the macOS one doesn't).

```bash name="by-hand" cache output="markdown"
SVG='<svg xmlns="http://www.w3.org/2000/svg" width="160" height="80" viewBox="0 0 160 80"><rect x="4" y="4" width="152" height="72" rx="10" fill="#ffd166" stroke="#444" stroke-width="3"/><text x="80" y="47" text-anchor="middle" font-family="sans-serif" font-size="18" fill="#222">output=markdown</text></svg>'
echo "Anything Markdown can go around the picture — a **caption**, a table, ..."
echo
printf '![hand-made card](data:image/svg+xml;base64,%s)\n' "$(printf '%s' "$SVG" | base64 | tr -d '\n')"
```
<!-- meshfox:output name="by-hand" hash="a9043d36" -->

Anything Markdown can go around the picture — a **caption**, a table, ...

![hand-made card](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIxNjAiIGhlaWdodD0iODAiIHZpZXdCb3g9IjAgMCAxNjAgODAiPjxyZWN0IHg9IjQiIHk9IjQiIHdpZHRoPSIxNTIiIGhlaWdodD0iNzIiIHJ4PSIxMCIgZmlsbD0iI2ZmZDE2NiIgc3Ryb2tlPSIjNDQ0IiBzdHJva2Utd2lkdGg9IjMiLz48dGV4dCB4PSI4MCIgeT0iNDciIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4IiBmaWxsPSIjMjIyIj5vdXRwdXQ9bWFya2Rvd248L3RleHQ+PC9zdmc+)

<!-- /meshfox:output -->

## output=image
<!-- meshfox:node id="output-image" -->

stdout is the SVG and nothing else; meshfox turns it into the image. No `base64`, no Markdown. stderr (warnings, progress) is still captured separately and shown as its own block above the picture.

```bash name="plain-svg" cache output="image"
echo "this goes to stderr, shown above the image" >&2
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="160" height="80" viewBox="0 0 160 80">
  <rect x="4" y="4" width="152" height="72" rx="10" fill="#9bd1a5" stroke="#444" stroke-width="3"/>
  <text x="80" y="47" text-anchor="middle" font-family="sans-serif" font-size="18" fill="#222">output=image</text>
</svg>
EOF
```
<!-- meshfox:output name="plain-svg" hash="66fac905" -->

```text
this goes to stderr, shown above the image
```

![plain-svg](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIxNjAiIGhlaWdodD0iODAiIHZpZXdCb3g9IjAgMCAxNjAgODAiPgogIDxyZWN0IHg9IjQiIHk9IjQiIHdpZHRoPSIxNTIiIGhlaWdodD0iNzIiIHJ4PSIxMCIgZmlsbD0iIzliZDFhNSIgc3Ryb2tlPSIjNDQ0IiBzdHJva2Utd2lkdGg9IjMiLz4KICA8dGV4dCB4PSI4MCIgeT0iNDciIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4IiBmaWxsPSIjMjIyIj5vdXRwdXQ9aW1hZ2U8L3RleHQ+Cjwvc3ZnPg==)

<!-- /meshfox:output -->

If the block fails, or prints something that isn't an SVG, there is no broken image: you see what it printed, as plain text (and, for a failure, the exit code).

```bash name="not-an-svg" cache output="image"
echo "the renderer said: unexpected token on line 3"
```
<!-- meshfox:output name="not-an-svg" hash="a25c867b" -->

```
the renderer said: unexpected token on line 3
```

<!-- /meshfox:output -->

## Theme
<!-- meshfox:node id="theme" -->

An SVG is shown through an `<img>` (web) or rasterized (TUI), where it can't see the page's CSS — so a diagram drawn in black vanishes on a dark theme. Right before showing it, meshfox rewrites the SVG's *text* (the file keeps exactly what the tool printed) so it can follow the theme, in two ways:

- `currentColor` becomes the theme's text color;
- `var(--mf-fg)`, `var(--mf-bg)`, `var(--mf-accent)`, `var(--mf-border)` become the theme's colors. Write a fallback — `var(--mf-fg, #000)` — and a viewer that knows nothing about meshfox (GitHub, a plain browser) still draws sensible colors.

Switch the web UI's theme (or look at the TUI, which is always dark) and compare these three. The first one is the problem; the other two are the fix.

**Hard-coded black** — doesn't follow anything:

```bash name="hardcoded" cache output="image"
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="56" viewBox="0 0 200 56">
  <rect x="2" y="2" width="196" height="52" rx="8" fill="none" stroke="#000" stroke-width="2"/>
  <text x="100" y="33" text-anchor="middle" font-family="sans-serif" font-size="16" fill="#000">hard-coded black</text>
</svg>
EOF
```
<!-- meshfox:output name="hardcoded" hash="3919a219" -->

![hardcoded](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMDAiIGhlaWdodD0iNTYiIHZpZXdCb3g9IjAgMCAyMDAgNTYiPgogIDxyZWN0IHg9IjIiIHk9IjIiIHdpZHRoPSIxOTYiIGhlaWdodD0iNTIiIHJ4PSI4IiBmaWxsPSJub25lIiBzdHJva2U9IiMwMDAiIHN0cm9rZS13aWR0aD0iMiIvPgogIDx0ZXh0IHg9IjEwMCIgeT0iMzMiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBmb250LXNpemU9IjE2IiBmaWxsPSIjMDAwIj5oYXJkLWNvZGVkIGJsYWNrPC90ZXh0Pgo8L3N2Zz4=)

<!-- /meshfox:output -->

**`currentColor`** — the text color of the theme:

```bash name="current-color" cache output="image"
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="56" viewBox="0 0 200 56">
  <rect x="2" y="2" width="196" height="52" rx="8" fill="none" stroke="currentColor" stroke-width="2"/>
  <text x="100" y="33" text-anchor="middle" font-family="sans-serif" font-size="16" fill="currentColor">currentColor</text>
</svg>
EOF
```
<!-- meshfox:output name="current-color" hash="ac72f3df" -->

![current-color](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMDAiIGhlaWdodD0iNTYiIHZpZXdCb3g9IjAgMCAyMDAgNTYiPgogIDxyZWN0IHg9IjIiIHk9IjIiIHdpZHRoPSIxOTYiIGhlaWdodD0iNTIiIHJ4PSI4IiBmaWxsPSJub25lIiBzdHJva2U9ImN1cnJlbnRDb2xvciIgc3Ryb2tlLXdpZHRoPSIyIi8+CiAgPHRleHQgeD0iMTAwIiB5PSIzMyIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9InNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTYiIGZpbGw9ImN1cnJlbnRDb2xvciI+Y3VycmVudENvbG9yPC90ZXh0Pgo8L3N2Zz4=)

<!-- /meshfox:output -->

**`--mf-*` variables** with fallbacks — a themed card with an accent dot:

```bash name="mf-vars" cache output="image"
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="56" viewBox="0 0 200 56">
  <rect x="2" y="2" width="196" height="52" rx="8" fill="var(--mf-bg, #fff)" stroke="var(--mf-border, #ccc)" stroke-width="2"/>
  <circle cx="22" cy="28" r="8" fill="var(--mf-accent, #ea580c)"/>
  <text x="40" y="33" font-family="sans-serif" font-size="16" fill="var(--mf-fg, #000)">--mf-* variables</text>
</svg>
EOF
```
<!-- meshfox:output name="mf-vars" hash="babcf48e" -->

![mf-vars](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMDAiIGhlaWdodD0iNTYiIHZpZXdCb3g9IjAgMCAyMDAgNTYiPgogIDxyZWN0IHg9IjIiIHk9IjIiIHdpZHRoPSIxOTYiIGhlaWdodD0iNTIiIHJ4PSI4IiBmaWxsPSJ2YXIoLS1tZi1iZywgI2ZmZikiIHN0cm9rZT0idmFyKC0tbWYtYm9yZGVyLCAjY2NjKSIgc3Ryb2tlLXdpZHRoPSIyIi8+CiAgPGNpcmNsZSBjeD0iMjIiIGN5PSIyOCIgcj0iOCIgZmlsbD0idmFyKC0tbWYtYWNjZW50LCAjZWE1ODBjKSIvPgogIDx0ZXh0IHg9IjQwIiB5PSIzMyIgZm9udC1mYW1pbHk9InNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTYiIGZpbGw9InZhcigtLW1mLWZnLCAjMDAwKSI+LS1tZi0qIHZhcmlhYmxlczwvdGV4dD4KPC9zdmc+)

<!-- /meshfox:output -->

Only the SVG's text is rewritten, and only for display; a tool's own explicit colors (`fill="#ffd166"`) are never touched. The SVG still goes through `<img>`, so scripts inside it stay inert.

## Background
<!-- meshfox:node id="background" -->

Many tools draw for a white page: transparent background, dark text, fixed colors that `currentColor`/`--mf-*` can't reach (Mermaid, PlantUML, a Typst formula). On a dark theme that is unreadable. Rather than a global rule, you pick a backing **per block** — `bg=` accepts `#rgb`, `#rrggbb` or `transparent`, and nothing else (not free-form CSS; the same few words work in the web UI, the TUI, the static site and the PDF).

With `output="image"`, put it in the fence's `output-attrs=` — the image-attribute syntax (`width=`/`height=` as an integer or `NN%`, `bg=`), forwarded onto the generated image:

```bash name="on-white" cache output="image" output-attrs="bg=#fff"
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="56" viewBox="0 0 200 56">
  <rect x="2" y="2" width="196" height="52" rx="8" fill="none" stroke="#000" stroke-width="2"/>
  <text x="100" y="33" text-anchor="middle" font-family="sans-serif" font-size="16" fill="#000">black on bg=#fff</text>
</svg>
EOF
```
<!-- meshfox:output name="on-white" hash="5c8164d7" -->

![on-white](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMDAiIGhlaWdodD0iNTYiIHZpZXdCb3g9IjAgMCAyMDAgNTYiPgogIDxyZWN0IHg9IjIiIHk9IjIiIHdpZHRoPSIxOTYiIGhlaWdodD0iNTIiIHJ4PSI4IiBmaWxsPSJub25lIiBzdHJva2U9IiMwMDAiIHN0cm9rZS13aWR0aD0iMiIvPgogIDx0ZXh0IHg9IjEwMCIgeT0iMzMiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBmb250LXNpemU9IjE2IiBmaWxsPSIjMDAwIj5ibGFjayBvbiBiZz0jZmZmPC90ZXh0Pgo8L3N2Zz4=){bg=#ffffff}

<!-- /meshfox:output -->

Combined with a size (`%` is the only form the TUI honours — a terminal has no pixel grid):

```bash name="on-white-half" cache output="image" output-attrs="width=50% bg=#fff"
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="400" height="112" viewBox="0 0 400 112">
  <rect x="4" y="4" width="392" height="104" rx="14" fill="#cfe8ff" stroke="#1d4e89" stroke-width="4"/>
  <text x="200" y="66" text-anchor="middle" font-family="sans-serif" font-size="30" fill="#1d4e89">width=50% bg=#fff</text>
</svg>
EOF
```
<!-- meshfox:output name="on-white-half" hash="68dfcb80" -->

![on-white-half](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSI0MDAiIGhlaWdodD0iMTEyIiB2aWV3Qm94PSIwIDAgNDAwIDExMiI+CiAgPHJlY3QgeD0iNCIgeT0iNCIgd2lkdGg9IjM5MiIgaGVpZ2h0PSIxMDQiIHJ4PSIxNCIgZmlsbD0iI2NmZThmZiIgc3Ryb2tlPSIjMWQ0ZTg5IiBzdHJva2Utd2lkdGg9IjQiLz4KICA8dGV4dCB4PSIyMDAiIHk9IjY2IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0ic2Fucy1zZXJpZiIgZm9udC1zaXplPSIzMCIgZmlsbD0iIzFkNGU4OSI+d2lkdGg9NTAlIGJnPSNmZmY8L3RleHQ+Cjwvc3ZnPg==){width=50% bg=#ffffff}

<!-- /meshfox:output -->

`transparent` says "no backing" explicitly (the default anyway):

```bash name="see-through" cache output="image" output-attrs="bg=transparent"
cat <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="56" viewBox="0 0 200 56">
  <rect x="2" y="2" width="196" height="52" rx="8" fill="none" stroke="currentColor" stroke-width="2"/>
  <text x="100" y="33" text-anchor="middle" font-family="sans-serif" font-size="16" fill="currentColor">bg=transparent</text>
</svg>
EOF
```
<!-- meshfox:output name="see-through" hash="c2152ee3" -->

![see-through](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMDAiIGhlaWdodD0iNTYiIHZpZXdCb3g9IjAgMCAyMDAgNTYiPgogIDxyZWN0IHg9IjIiIHk9IjIiIHdpZHRoPSIxOTYiIGhlaWdodD0iNTIiIHJ4PSI4IiBmaWxsPSJub25lIiBzdHJva2U9ImN1cnJlbnRDb2xvciIgc3Ryb2tlLXdpZHRoPSIyIi8+CiAgPHRleHQgeD0iMTAwIiB5PSIzMyIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9InNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTYiIGZpbGw9ImN1cnJlbnRDb2xvciI+Ymc9dHJhbnNwYXJlbnQ8L3RleHQ+Cjwvc3ZnPg==){bg=transparent}

<!-- /meshfox:output -->

With `output="markdown"` — or in any hand-written Markdown — it's the same syntax straight after the image, no space: `![alt](url){bg=#fff}` (also `{width=50%}`, `{width=50% bg=#fff}`):

```bash name="markdown-with-bg" cache output="markdown"
SVG='<svg xmlns="http://www.w3.org/2000/svg" width="200" height="56" viewBox="0 0 200 56"><rect x="2" y="2" width="196" height="52" rx="8" fill="none" stroke="#000" stroke-width="2"/><text x="100" y="33" text-anchor="middle" font-family="sans-serif" font-size="16" fill="#000">{bg=#fff} in Markdown</text></svg>'
printf '![black on white](data:image/svg+xml;base64,%s){bg=#fff}\n' "$(printf '%s' "$SVG" | base64 | tr -d '\n')"
```
<!-- meshfox:output name="markdown-with-bg" hash="2f9a54be" -->

![black on white](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyMDAiIGhlaWdodD0iNTYiIHZpZXdCb3g9IjAgMCAyMDAgNTYiPjxyZWN0IHg9IjIiIHk9IjIiIHdpZHRoPSIxOTYiIGhlaWdodD0iNTIiIHJ4PSI4IiBmaWxsPSJub25lIiBzdHJva2U9IiMwMDAiIHN0cm9rZS13aWR0aD0iMiIvPjx0ZXh0IHg9IjEwMCIgeT0iMzMiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBmb250LXNpemU9IjE2IiBmaWxsPSIjMDAwIj57Ymc9I2ZmZn0gaW4gTWFya2Rvd248L3RleHQ+PC9zdmc+){bg=#fff}

<!-- /meshfox:output -->

`meshfox validate` rejects an `output-attrs=` that isn't in this grammar (`bg=white`, `color=red`) and one used without `output="image"`.

## Real renderers
<!-- meshfox:node id="real-renderers" -->

The same `output="image"` recipe with real tools. None of them is bundled with meshfox, so each block first checks that its tool is on `PATH` and says so if not — pick the ones you use. All of them print an SVG to stdout, which is all `output="image"` needs; the `bg=#fff` is there because these tools draw for a white page (see **Background**).

**Graphviz** — a graph from `dot`:

```bash name="graphviz" cache output="image" output-attrs="bg=#fff"
command -v dot >/dev/null || { echo "graphviz is not installed"; exit 1; }
dot -Tsvg <<'EOF'
digraph { rankdir=LR; node [shape=box, style=rounded]; canvas -> block -> svg -> picture }
EOF
```

**Typst** — a formula, rendered by Typst's own math engine. Its glyphs are outlines, so no font is needed to view it. Typst's math syntax, not LaTeX's:

```bash name="typst-math" cache output="image" output-attrs="bg=#fff"
command -v typst >/dev/null || { echo "typst is not installed"; exit 1; }
typst compile --format svg - - <<'EOF'
#set page(width: auto, height: auto, margin: 8pt)
$ integral_0^oo e^(-x^2) dif x = sqrt(pi) / 2 $
EOF
```

**Mermaid** (`npm i -g @mermaid-js/mermaid-cli`; needs a headless Chromium) — `mmdc` writes the SVG where `-o` says, here stdout:

```bash name="mermaid" cache output="image" output-attrs="bg=#fff"
command -v mmdc >/dev/null || { echo "mermaid-cli is not installed (npm i -g @mermaid-js/mermaid-cli)"; exit 1; }
mmdc -q -i - -o /dev/stdout -e svg <<'EOF'
flowchart LR
  A[Block] -->|prints SVG| B(output=image) --> C{{Picture}}
EOF
```

**PlantUML** (needs Java) — `-pipe` reads stdin and writes stdout:

```bash name="plantuml" cache output="image" output-attrs="bg=#fff"
command -v plantuml >/dev/null || { echo "plantuml is not installed"; exit 1; }
plantuml -tsvg -pipe <<'EOF'
@startuml
Alice -> Bob: SVG, please
Bob --> Alice: <svg ...>
@enduml
EOF
```

**Vega-Lite** (`npm i -g vega-lite vega-cli`) — a chart: `vl2vg` compiles the spec, `vg2svg` draws it:

```bash name="vega-lite" cache output="image" output-attrs="bg=#fff"
command -v vl2vg >/dev/null && command -v vg2svg >/dev/null || { echo "vega is not installed (npm i -g vega-lite vega-cli)"; exit 1; }
vl2vg <<'EOF' | vg2svg
{
  "$schema": "https://vega.github.io/schema/vega-lite/v5.json",
  "data": {"values": [{"x": "a", "y": 3}, {"x": "b", "y": 7}, {"x": "c", "y": 5}]},
  "mark": "bar",
  "encoding": {"x": {"field": "x", "type": "nominal"}, "y": {"field": "y", "type": "quantitative"}}
}
EOF
```

**matplotlib** — `savefig(..., format="svg")` to a buffer, printed as the SVG (compare the PNG-in-data-URL version in [pandas-dataframe.canvas.md](./pandas-dataframe.canvas.md)). One self-contained block: it creates a private virtualenv next to the canvas on the first run (the same `.meshfox/<canvas>.venv` place meshfox's `@python_venv` would use), installs matplotlib into it, and runs the script there. All of that setup chatter goes to stderr, so stdout stays the SVG alone:

```bash name="matplotlib" cache output="image" output-attrs="bg=#fff"
set -euo pipefail
VENV=".meshfox/svg.canvas.md.venv"
[ -x "$VENV/bin/python" ] || python3 -m venv "$VENV" >&2
"$VENV/bin/python" -c "import matplotlib" 2>/dev/null || "$VENV/bin/python" -m pip install -q --disable-pip-version-check matplotlib >&2
"$VENV/bin/python" - <<'PY'
import io

import matplotlib

matplotlib.use("Agg")  # render to a buffer; no display needed
import matplotlib.pyplot as plt

fig, ax = plt.subplots(figsize=(4, 2.2))
ax.bar(["a", "b", "c"], [3, 7, 5], color="#5b8def")
fig.tight_layout()
buf = io.StringIO()
fig.savefig(buf, format="svg")
print(buf.getvalue())
PY
```
<!-- meshfox:output name="matplotlib" hash="07f85e2c" -->

![matplotlib](data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBlbmNvZGluZz0idXRmLTgiIHN0YW5kYWxvbmU9Im5vIj8+CjwhRE9DVFlQRSBzdmcgUFVCTElDICItLy9XM0MvL0RURCBTVkcgMS4xLy9FTiIKICAiaHR0cDovL3d3dy53My5vcmcvR3JhcGhpY3MvU1ZHLzEuMS9EVEQvc3ZnMTEuZHRkIj4KPHN2ZyB4bWxuczp4bGluaz0iaHR0cDovL3d3dy53My5vcmcvMTk5OS94bGluayIgd2lkdGg9IjI4OHB0IiBoZWlnaHQ9IjE1OC40cHQiIHZpZXdCb3g9IjAgMCAyODggMTU4LjQiIHhtbG5zPSJodHRwOi8vd3d3LnczLm9yZy8yMDAwL3N2ZyIgdmVyc2lvbj0iMS4xIj4KIDxtZXRhZGF0YT4KICA8cmRmOlJERiB4bWxuczpkYz0iaHR0cDovL3B1cmwub3JnL2RjL2VsZW1lbnRzLzEuMS8iIHhtbG5zOmNjPSJodHRwOi8vY3JlYXRpdmVjb21tb25zLm9yZy9ucyMiIHhtbG5zOnJkZj0iaHR0cDovL3d3dy53My5vcmcvMTk5OS8wMi8yMi1yZGYtc3ludGF4LW5zIyI+CiAgIDxjYzpXb3JrPgogICAgPGRjOnR5cGUgcmRmOnJlc291cmNlPSJodHRwOi8vcHVybC5vcmcvZGMvZGNtaXR5cGUvU3RpbGxJbWFnZSIvPgogICAgPGRjOmRhdGU+MjAyNi0xMC0wMlQyMDo1NDozMS45MzA5NTU8L2RjOmRhdGU+CiAgICA8ZGM6Zm9ybWF0PmltYWdlL3N2Zyt4bWw8L2RjOmZvcm1hdD4KICAgIDxkYzpjcmVhdG9yPgogICAgIDxjYzpBZ2VudD4KICAgICAgPGRjOnRpdGxlPk1hdHBsb3RsaWIgdjMuMTEuMiwgaHR0cHM6Ly9tYXRwbG90bGliLm9yZy88L2RjOnRpdGxlPgogICAgIDwvY2M6QWdlbnQ+CiAgICA8L2RjOmNyZWF0b3I+CiAgIDwvY2M6V29yaz4KICA8L3JkZjpSREY+CiA8L21ldGFkYXRhPgogPGRlZnM+CiAgPHN0eWxlIHR5cGU9InRleHQvY3NzIj4qe3N0cm9rZS1saW5lam9pbjogcm91bmQ7IHN0cm9rZS1saW5lY2FwOiBidXR0fTwvc3R5bGU+CiA8L2RlZnM+CiA8ZyBpZD0iZmlndXJlXzEiPgogIDxnIGlkPSJwYXRjaF8xIj4KICAgPHBhdGggZD0iTSAwIDE1OC40IApMIDI4OCAxNTguNCAKTCAyODggMCAKTCAwIDAgCnoKIiBzdHlsZT0iZmlsbDogI2ZmZmZmZiIvPgogIDwvZz4KICA8ZyBpZD0iYXhlc18xIj4KICAgPGcgaWQ9InBhdGNoXzIiPgogICAgPHBhdGggZD0iTSAyNC4yOCAxMzAuMjc3NjU2IApMIDI3Ny4yIDEzMC4yNzc2NTYgCkwgMjc3LjIgMTAuOCAKTCAyNC4yOCAxMC44IAp6CiIgc3R5bGU9ImZpbGw6ICNmZmZmZmYiLz4KICAgPC9nPgogICA8ZyBpZD0icGF0Y2hfMyI+CiAgICA8cGF0aCBkPSJNIDM1Ljc3NjM2NCAxMzAuMjc3NjU2IApMIDEwMS40Njk4NyAxMzAuMjc3NjU2IApMIDEwMS40Njk4NyA4MS41MTEyNjYgCkwgMzUuNzc2MzY0IDgxLjUxMTI2NiAKegoiIGNsaXAtcGF0aD0idXJsKCNwMzg1NTEzOGYzYikiIHN0eWxlPSJmaWxsOiAjNWI4ZGVmIi8+CiAgIDwvZz4KICAgPGcgaWQ9InBhdGNoXzQiPgogICAgPHBhdGggZD0iTSAxMTcuODkzMjQ3IDEzMC4yNzc2NTYgCkwgMTgzLjU4Njc1MyAxMzAuMjc3NjU2IApMIDE4My41ODY3NTMgMTYuNDg5NDEyIApMIDExNy44OTMyNDcgMTYuNDg5NDEyIAp6CiIgY2xpcC1wYXRoPSJ1cmwoI3AzODU1MTM4ZjNiKSIgc3R5bGU9ImZpbGw6ICM1YjhkZWYiLz4KICAgPC9nPgogICA8ZyBpZD0icGF0Y2hfNSI+CiAgICA8cGF0aCBkPSJNIDIwMC4wMTAxMyAxMzAuMjc3NjU2IApMIDI2NS43MDM2MzYgMTMwLjI3NzY1NiAKTCAyNjUuNzAzNjM2IDQ5LjAwMDMzOSAKTCAyMDAuMDEwMTMgNDkuMDAwMzM5IAp6CiIgY2xpcC1wYXRoPSJ1cmwoI3AzODU1MTM4ZjNiKSIgc3R5bGU9ImZpbGw6ICM1YjhkZWYiLz4KICAgPC9nPgogICA8ZyBpZD0ibWF0cGxvdGxpYi5heGlzXzEiPgogICAgPGcgaWQ9Inh0aWNrXzEiPgogICAgIDxnIGlkPSJsaW5lMmRfMSI+CiAgICAgIDxkZWZzPgogICAgICAgPHBhdGggaWQ9Im1jYmU5YTZiNjQ3IiBkPSJNIDAgMCAKTCAwIDMuNSAKIiBzdHlsZT0ic3Ryb2tlOiAjMDAwMDAwOyBzdHJva2Utd2lkdGg6IDAuOCIvPgogICAgICA8L2RlZnM+CiAgICAgIDxnPgogICAgICAgPHVzZSB4bGluazpocmVmPSIjbWNiZTlhNmI2NDciIHg9IjY4LjYyMzExNyIgeT0iMTMwLjI3NzY1NiIgc3R5bGU9InN0cm9rZTogIzAwMDAwMDsgc3Ryb2tlLXdpZHRoOiAwLjgiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgICA8ZyBpZD0idGV4dF8xIj4KICAgICAgPCEtLSBhIC0tPgogICAgICA8ZyB0cmFuc2Zvcm09InRyYW5zbGF0ZSg2NS41NTkwNTQgMTQ0Ljg3NTMxMykgc2NhbGUoMC4xIC0wLjEpIj4KICAgICAgIDxkZWZzPgogICAgICAgIDxwYXRoIGlkPSJEZWphVnVTYW5zLTQ0IiBkPSJNIDIxOTQgMTc1OSAKUSAxNDk3IDE3NTkgMTIyOCAxNjAwIApRIDk1OSAxNDQxIDk1OSAxMDU2IApRIDk1OSA3NTAgMTE2MSA1NzAgClEgMTM2MyAzOTEgMTcwOSAzOTEgClEgMjE4OCAzOTEgMjQ3NyA3MzAgClEgMjc2NiAxMDY5IDI3NjYgMTYzMSAKTCAyNzY2IDE3NTkgCkwgMjE5NCAxNzU5IAp6Ck0gMzM0MSAxOTk3IApMIDMzNDEgMCAKTCAyNzY2IDAgCkwgMjc2NiA1MzEgClEgMjU2OSAyMTMgMjI3NSA2MSAKUSAxOTgxIC05MSAxNTU2IC05MSAKUSAxMDE5IC05MSA3MDEgMjExIApRIDM4NCA1MTMgMzg0IDEwMTkgClEgMzg0IDE2MDkgNzc5IDE5MDkgClEgMTE3NSAyMjA5IDE5NTkgMjIwOSAKTCAyNzY2IDIyMDkgCkwgMjc2NiAyMjY2IApRIDI3NjYgMjY2MyAyNTA1IDI4ODAgClEgMjI0NCAzMDk3IDE3NzIgMzA5NyAKUSAxNDcyIDMwOTcgMTE4NyAzMDI1IApRIDkwMyAyOTUzIDY0MSAyODA5IApMIDY0MSAzMzQxIApRIDk1NiAzNDYzIDEyNTMgMzUyMyAKUSAxNTUwIDM1ODQgMTgzMSAzNTg0IApRIDI1OTEgMzU4NCAyOTY2IDMxOTAgClEgMzM0MSAyNzk3IDMzNDEgMTk5NyAKegoiIHRyYW5zZm9ybT0ic2NhbGUoMC4wMTU2MjUpIi8+CiAgICAgICA8L2RlZnM+CiAgICAgICA8dXNlIHhsaW5rOmhyZWY9IiNEZWphVnVTYW5zLTQ0Ii8+CiAgICAgIDwvZz4KICAgICA8L2c+CiAgICA8L2c+CiAgICA8ZyBpZD0ieHRpY2tfMiI+CiAgICAgPGcgaWQ9ImxpbmUyZF8yIj4KICAgICAgPGc+CiAgICAgICA8dXNlIHhsaW5rOmhyZWY9IiNtY2JlOWE2YjY0NyIgeD0iMTUwLjc0IiB5PSIxMzAuMjc3NjU2IiBzdHlsZT0ic3Ryb2tlOiAjMDAwMDAwOyBzdHJva2Utd2lkdGg6IDAuOCIvPgogICAgICA8L2c+CiAgICAgPC9nPgogICAgIDxnIGlkPSJ0ZXh0XzIiPgogICAgICA8IS0tIGIgLS0+CiAgICAgIDxnIHRyYW5zZm9ybT0idHJhbnNsYXRlKDE0Ny41NjU3ODEgMTQ0Ljg3NjA5NCkgc2NhbGUoMC4xIC0wLjEpIj4KICAgICAgIDxkZWZzPgogICAgICAgIDxwYXRoIGlkPSJEZWphVnVTYW5zLTQ1IiBkPSJNIDMxMTYgMTc0NyAKUSAzMTE2IDIzODEgMjg1NSAyNzQyIApRIDI1OTQgMzEwMyAyMTM4IDMxMDMgClEgMTY4MSAzMTAzIDE0MjAgMjc0MiAKUSAxMTU5IDIzODEgMTE1OSAxNzQ3IApRIDExNTkgMTExMyAxNDIwIDc1MiAKUSAxNjgxIDM5MSAyMTM4IDM5MSAKUSAyNTk0IDM5MSAyODU1IDc1MiAKUSAzMTE2IDExMTMgMzExNiAxNzQ3IAp6Ck0gMTE1OSAyOTY5IApRIDEzNDEgMzI4MSAxNjE3IDM0MzIgClEgMTg5NCAzNTg0IDIyNzggMzU4NCAKUSAyOTE2IDM1ODQgMzMxNCAzMDc4IApRIDM3MTMgMjU3MiAzNzEzIDE3NDcgClEgMzcxMyA5MjIgMzMxNCA0MTUgClEgMjkxNiAtOTEgMjI3OCAtOTEgClEgMTg5NCAtOTEgMTYxNyA2MSAKUSAxMzQxIDIxMyAxMTU5IDUyNSAKTCAxMTU5IDAgCkwgNTgxIDAgCkwgNTgxIDQ4NjMgCkwgMTE1OSA0ODYzIApMIDExNTkgMjk2OSAKegoiIHRyYW5zZm9ybT0ic2NhbGUoMC4wMTU2MjUpIi8+CiAgICAgICA8L2RlZnM+CiAgICAgICA8dXNlIHhsaW5rOmhyZWY9IiNEZWphVnVTYW5zLTQ1Ii8+CiAgICAgIDwvZz4KICAgICA8L2c+CiAgICA8L2c+CiAgICA8ZyBpZD0ieHRpY2tfMyI+CiAgICAgPGcgaWQ9ImxpbmUyZF8zIj4KICAgICAgPGc+CiAgICAgICA8dXNlIHhsaW5rOmhyZWY9IiNtY2JlOWE2YjY0NyIgeD0iMjMyLjg1Njg4MyIgeT0iMTMwLjI3NzY1NiIgc3R5bGU9InN0cm9rZTogIzAwMDAwMDsgc3Ryb2tlLXdpZHRoOiAwLjgiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgICA8ZyBpZD0idGV4dF8zIj4KICAgICAgPCEtLSBjIC0tPgogICAgICA8ZyB0cmFuc2Zvcm09InRyYW5zbGF0ZSgyMzAuMTA3NjY0IDE0NC44NzUzMTMpIHNjYWxlKDAuMSAtMC4xKSI+CiAgICAgICA8ZGVmcz4KICAgICAgICA8cGF0aCBpZD0iRGVqYVZ1U2Fucy00NiIgZD0iTSAzMTIyIDMzNjYgCkwgMzEyMiAyODI4IApRIDI4NzggMjk2MyAyNjMzIDMwMzAgClEgMjM4OCAzMDk3IDIxMzggMzA5NyAKUSAxNTc4IDMwOTcgMTI2OCAyNzQyIApRIDk1OSAyMzg4IDk1OSAxNzQ3IApRIDk1OSAxMTA2IDEyNjggNzUxIApRIDE1NzggMzk3IDIxMzggMzk3IApRIDIzODggMzk3IDI2MzMgNDY0IApRIDI4NzggNTMxIDMxMjIgNjY2IApMIDMxMjIgMTM0IApRIDI4ODEgMjIgMjYyMyAtMzQgClEgMjM2NiAtOTEgMjA3NSAtOTEgClEgMTI4NCAtOTEgODE4IDQwNiAKUSAzNTMgOTAzIDM1MyAxNzQ3IApRIDM1MyAyNjAzIDgyMyAzMDkzIApRIDEyOTQgMzU4NCAyMTEzIDM1ODQgClEgMjM3OCAzNTg0IDI2MzEgMzUyOSAKUSAyODg0IDM0NzUgMzEyMiAzMzY2IAp6CiIgdHJhbnNmb3JtPSJzY2FsZSgwLjAxNTYyNSkiLz4KICAgICAgIDwvZGVmcz4KICAgICAgIDx1c2UgeGxpbms6aHJlZj0iI0RlamFWdVNhbnMtNDYiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgIDwvZz4KICAgPC9nPgogICA8ZyBpZD0ibWF0cGxvdGxpYi5heGlzXzIiPgogICAgPGcgaWQ9Inl0aWNrXzEiPgogICAgIDxnIGlkPSJsaW5lMmRfNCI+CiAgICAgIDxkZWZzPgogICAgICAgPHBhdGggaWQ9Im1kMmU0ZjZhNTRhIiBkPSJNIDAgMCAKTCAtMy41IDAgCiIgc3R5bGU9InN0cm9rZTogIzAwMDAwMDsgc3Ryb2tlLXdpZHRoOiAwLjgiLz4KICAgICAgPC9kZWZzPgogICAgICA8Zz4KICAgICAgIDx1c2UgeGxpbms6aHJlZj0iI21kMmU0ZjZhNTRhIiB4PSIyNC4yOCIgeT0iMTMwLjI3NzY1NiIgc3R5bGU9InN0cm9rZTogIzAwMDAwMDsgc3Ryb2tlLXdpZHRoOiAwLjgiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgICA8ZyBpZD0idGV4dF80Ij4KICAgICAgPCEtLSAwIC0tPgogICAgICA8ZyB0cmFuc2Zvcm09InRyYW5zbGF0ZSgxMC45MTc1IDEzNC4wNzY0ODQpIHNjYWxlKDAuMSAtMC4xKSI+CiAgICAgICA8ZGVmcz4KICAgICAgICA8cGF0aCBpZD0iRGVqYVZ1U2Fucy0xMyIgZD0iTSAyMDM0IDQyNTAgClEgMTU0NyA0MjUwIDEzMDEgMzc3MCAKUSAxMDU2IDMyOTEgMTA1NiAyMzI4IApRIDEwNTYgMTM2OSAxMzAxIDg4OSAKUSAxNTQ3IDQwOSAyMDM0IDQwOSAKUSAyNTI1IDQwOSAyNzcwIDg4OSAKUSAzMDE2IDEzNjkgMzAxNiAyMzI4IApRIDMwMTYgMzI5MSAyNzcwIDM3NzAgClEgMjUyNSA0MjUwIDIwMzQgNDI1MCAKegpNIDIwMzQgNDc1MCAKUSAyODE5IDQ3NTAgMzIzMyA0MTI5IApRIDM2NDcgMzUwOSAzNjQ3IDIzMjggClEgMzY0NyAxMTUwIDMyMzMgNTI5IApRIDI4MTkgLTkxIDIwMzQgLTkxIApRIDEyNTAgLTkxIDgzNiA1MjkgClEgNDIyIDExNTAgNDIyIDIzMjggClEgNDIyIDM1MDkgODM2IDQxMjkgClEgMTI1MCA0NzUwIDIwMzQgNDc1MCAKegoiIHRyYW5zZm9ybT0ic2NhbGUoMC4wMTU2MjUpIi8+CiAgICAgICA8L2RlZnM+CiAgICAgICA8dXNlIHhsaW5rOmhyZWY9IiNEZWphVnVTYW5zLTEzIi8+CiAgICAgIDwvZz4KICAgICA8L2c+CiAgICA8L2c+CiAgICA8ZyBpZD0ieXRpY2tfMiI+CiAgICAgPGcgaWQ9ImxpbmUyZF81Ij4KICAgICAgPGc+CiAgICAgICA8dXNlIHhsaW5rOmhyZWY9IiNtZDJlNGY2YTU0YSIgeD0iMjQuMjgiIHk9Ijk3Ljc2NjcyOSIgc3R5bGU9InN0cm9rZTogIzAwMDAwMDsgc3Ryb2tlLXdpZHRoOiAwLjgiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgICA8ZyBpZD0idGV4dF81Ij4KICAgICAgPCEtLSAyIC0tPgogICAgICA8ZyB0cmFuc2Zvcm09InRyYW5zbGF0ZSgxMC45MTc1IDEwMS41NjU1NTgpIHNjYWxlKDAuMSAtMC4xKSI+CiAgICAgICA8ZGVmcz4KICAgICAgICA8cGF0aCBpZD0iRGVqYVZ1U2Fucy0xNSIgZD0iTSAxMjI4IDUzMSAKTCAzNDMxIDUzMSAKTCAzNDMxIDAgCkwgNDY5IDAgCkwgNDY5IDUzMSAKUSA4MjggOTAzIDE0NDggMTUyOSAKUSAyMDY5IDIxNTYgMjIyOCAyMzM4IApRIDI1MzEgMjY3OCAyNjUxIDI5MTQgClEgMjc3MiAzMTUwIDI3NzIgMzM3OCAKUSAyNzcyIDM3NTAgMjUxMSAzOTg0IApRIDIyNTAgNDIxOSAxODMxIDQyMTkgClEgMTUzNCA0MjE5IDEyMDQgNDExNiAKUSA4NzUgNDAxMyA1MDAgMzgwMyAKTCA1MDAgNDQ0MSAKUSA4ODEgNDU5NCAxMjEyIDQ2NzIgClEgMTU0NCA0NzUwIDE4MTkgNDc1MCAKUSAyNTQ0IDQ3NTAgMjk3NSA0Mzg3IApRIDM0MDYgNDAyNSAzNDA2IDM0MTkgClEgMzQwNiAzMTMxIDMyOTggMjg3MyAKUSAzMTkxIDI2MTYgMjkwNiAyMjY2IApRIDI4MjggMjE3NSAyNDA5IDE3NDIgClEgMTk5MSAxMzA5IDEyMjggNTMxIAp6CiIgdHJhbnNmb3JtPSJzY2FsZSgwLjAxNTYyNSkiLz4KICAgICAgIDwvZGVmcz4KICAgICAgIDx1c2UgeGxpbms6aHJlZj0iI0RlamFWdVNhbnMtMTUiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgIDwvZz4KICAgIDxnIGlkPSJ5dGlja18zIj4KICAgICA8ZyBpZD0ibGluZTJkXzYiPgogICAgICA8Zz4KICAgICAgIDx1c2UgeGxpbms6aHJlZj0iI21kMmU0ZjZhNTRhIiB4PSIyNC4yOCIgeT0iNjUuMjU1ODAzIiBzdHlsZT0ic3Ryb2tlOiAjMDAwMDAwOyBzdHJva2Utd2lkdGg6IDAuOCIvPgogICAgICA8L2c+CiAgICAgPC9nPgogICAgIDxnIGlkPSJ0ZXh0XzYiPgogICAgICA8IS0tIDQgLS0+CiAgICAgIDxnIHRyYW5zZm9ybT0idHJhbnNsYXRlKDEwLjkxNzUgNjkuMDU0NjMxKSBzY2FsZSgwLjEgLTAuMSkiPgogICAgICAgPGRlZnM+CiAgICAgICAgPHBhdGggaWQ9IkRlamFWdVNhbnMtMTciIGQ9Ik0gMjQxOSA0MTE2IApMIDgyNSAxNjI1IApMIDI0MTkgMTYyNSAKTCAyNDE5IDQxMTYgCnoKTSAyMjUzIDQ2NjYgCkwgMzA0NyA0NjY2IApMIDMwNDcgMTYyNSAKTCAzNzEzIDE2MjUgCkwgMzcxMyAxMTAwIApMIDMwNDcgMTEwMCAKTCAzMDQ3IDAgCkwgMjQxOSAwIApMIDI0MTkgMTEwMCAKTCAzMTMgMTEwMCAKTCAzMTMgMTcwOSAKTCAyMjUzIDQ2NjYgCnoKIiB0cmFuc2Zvcm09InNjYWxlKDAuMDE1NjI1KSIvPgogICAgICAgPC9kZWZzPgogICAgICAgPHVzZSB4bGluazpocmVmPSIjRGVqYVZ1U2Fucy0xNyIvPgogICAgICA8L2c+CiAgICAgPC9nPgogICAgPC9nPgogICAgPGcgaWQ9Inl0aWNrXzQiPgogICAgIDxnIGlkPSJsaW5lMmRfNyI+CiAgICAgIDxnPgogICAgICAgPHVzZSB4bGluazpocmVmPSIjbWQyZTRmNmE1NGEiIHg9IjI0LjI4IiB5PSIzMi43NDQ4NzYiIHN0eWxlPSJzdHJva2U6ICMwMDAwMDA7IHN0cm9rZS13aWR0aDogMC44Ii8+CiAgICAgIDwvZz4KICAgICA8L2c+CiAgICAgPGcgaWQ9InRleHRfNyI+CiAgICAgIDwhLS0gNiAtLT4KICAgICAgPGcgdHJhbnNmb3JtPSJ0cmFuc2xhdGUoMTAuOTE3NSAzNi41NDM3MDQpIHNjYWxlKDAuMSAtMC4xKSI+CiAgICAgICA8ZGVmcz4KICAgICAgICA8cGF0aCBpZD0iRGVqYVZ1U2Fucy0xOSIgZD0iTSAyMTEzIDI1ODQgClEgMTY4OCAyNTg0IDE0MzkgMjI5MyAKUSAxMTkxIDIwMDMgMTE5MSAxNDk3IApRIDExOTEgOTk0IDE0MzkgNzAxIApRIDE2ODggNDA5IDIxMTMgNDA5IApRIDI1MzggNDA5IDI3ODYgNzAxIApRIDMwMzQgOTk0IDMwMzQgMTQ5NyAKUSAzMDM0IDIwMDMgMjc4NiAyMjkzIApRIDI1MzggMjU4NCAyMTEzIDI1ODQgCnoKTSAzMzY2IDQ1NjMgCkwgMzM2NiAzOTg4IApRIDMxMjggNDEwMCAyODg2IDQxNTkgClEgMjY0NCA0MjE5IDI0MDYgNDIxOSAKUSAxNzgxIDQyMTkgMTQ1MSAzNzk3IApRIDExMjIgMzM3NSAxMDc1IDI1MjIgClEgMTI1OSAyNzk0IDE1MzcgMjkzOSAKUSAxODE2IDMwODQgMjE1MCAzMDg0IApRIDI4NTMgMzA4NCAzMjYxIDI2NTcgClEgMzY2OSAyMjMxIDM2NjkgMTQ5NyAKUSAzNjY5IDc3OCAzMjQ0IDM0MyAKUSAyODE5IC05MSAyMTEzIC05MSAKUSAxMzAzIC05MSA4NzUgNTI5IApRIDQ0NyAxMTUwIDQ0NyAyMzI4IApRIDQ0NyAzNDM0IDk3MiA0MDkyIApRIDE0OTcgNDc1MCAyMzgxIDQ3NTAgClEgMjYxOSA0NzUwIDI4NjEgNDcwMyAKUSAzMTAzIDQ2NTYgMzM2NiA0NTYzIAp6CiIgdHJhbnNmb3JtPSJzY2FsZSgwLjAxNTYyNSkiLz4KICAgICAgIDwvZGVmcz4KICAgICAgIDx1c2UgeGxpbms6aHJlZj0iI0RlamFWdVNhbnMtMTkiLz4KICAgICAgPC9nPgogICAgIDwvZz4KICAgIDwvZz4KICAgPC9nPgogICA8ZyBpZD0icGF0Y2hfNiI+CiAgICA8cGF0aCBkPSJNIDI0LjI4IDEzMC4yNzc2NTYgCkwgMjQuMjggMTAuOCAKIiBzdHlsZT0iZmlsbDogbm9uZTsgc3Ryb2tlOiAjMDAwMDAwOyBzdHJva2Utd2lkdGg6IDAuODsgc3Ryb2tlLWxpbmVqb2luOiBtaXRlcjsgc3Ryb2tlLWxpbmVjYXA6IHNxdWFyZSIvPgogICA8L2c+CiAgIDxnIGlkPSJwYXRjaF83Ij4KICAgIDxwYXRoIGQ9Ik0gMjc3LjIgMTMwLjI3NzY1NiAKTCAyNzcuMiAxMC44IAoiIHN0eWxlPSJmaWxsOiBub25lOyBzdHJva2U6ICMwMDAwMDA7IHN0cm9rZS13aWR0aDogMC44OyBzdHJva2UtbGluZWpvaW46IG1pdGVyOyBzdHJva2UtbGluZWNhcDogc3F1YXJlIi8+CiAgIDwvZz4KICAgPGcgaWQ9InBhdGNoXzgiPgogICAgPHBhdGggZD0iTSAyNC4yOCAxMzAuMjc3NjU2IApMIDI3Ny4yIDEzMC4yNzc2NTYgCiIgc3R5bGU9ImZpbGw6IG5vbmU7IHN0cm9rZTogIzAwMDAwMDsgc3Ryb2tlLXdpZHRoOiAwLjg7IHN0cm9rZS1saW5lam9pbjogbWl0ZXI7IHN0cm9rZS1saW5lY2FwOiBzcXVhcmUiLz4KICAgPC9nPgogICA8ZyBpZD0icGF0Y2hfOSI+CiAgICA8cGF0aCBkPSJNIDI0LjI4IDEwLjggCkwgMjc3LjIgMTAuOCAKIiBzdHlsZT0iZmlsbDogbm9uZTsgc3Ryb2tlOiAjMDAwMDAwOyBzdHJva2Utd2lkdGg6IDAuODsgc3Ryb2tlLWxpbmVqb2luOiBtaXRlcjsgc3Ryb2tlLWxpbmVjYXA6IHNxdWFyZSIvPgogICA8L2c+CiAgPC9nPgogPC9nPgogPGRlZnM+CiAgPGNsaXBQYXRoIGlkPSJwMzg1NTEzOGYzYiI+CiAgIDxyZWN0IHg9IjI0LjI4IiB5PSIxMC44IiB3aWR0aD0iMjUyLjkyIiBoZWlnaHQ9IjExOS40Nzc2NTYiLz4KICA8L2NsaXBQYXRoPgogPC9kZWZzPgo8L3N2Zz4=){bg=#ffffff}

<!-- /meshfox:output -->

