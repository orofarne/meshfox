<!-- meshfox:canvas -->
# summary-to-canvas

<!-- meshfox:comment -->
> This is a [meshfox](https://meshfox.orofarne.net/) document — open it
> with `meshfox view` (or `meshfox tui`) for the interactive canvas.
> This note is only visible here, in a plain Markdown viewer.
<!-- /meshfox:comment -->

A skill for regular Claude (claude.ai / desktop, not Claude Code): ask it to summarize a conversation as a meshfox canvas and it answers with a ready `.canvas.md` file. Build the zip below, upload it once, then use it in any chat.

## What's in the skill
<!-- meshfox:node id="contents" -->

| File | Role |
|---|---|
| `SKILL.md` | the instructions: output contract, the small slice of the format that matters, how to summarize, a final checklist |
| `example.canvas.md` | one worked canvas Claude imitates |

This README is not part of the skill and stays out of the zip.

## Build the zip
<!-- meshfox:node id="build" color="4" -->

Packs `SKILL.md` and `example.canvas.md` under a top-level `summary-to-canvas/` folder, the layout claude.ai expects, into `../summary-to-canvas.zip` (git-ignored). Validates the example first, so a broken one never ships.

```bash name="build" inputs="SKILL.md,example.canvas.md" outputs="../summary-to-canvas.zip"
meshfox validate example.canvas.md
cd ..
rm -f summary-to-canvas.zip
zip -X summary-to-canvas.zip \
  summary-to-canvas/SKILL.md \
  summary-to-canvas/example.canvas.md
unzip -l summary-to-canvas.zip
```

## Install in claude.ai
<!-- meshfox:node id="install" color="5" -->
<!-- meshfox:edge from="build" label="zip" -->

1. Open **Settings → Capabilities** and make sure *Skills* (and code execution / file creation) are enabled.
2. In **Skills**, choose **Upload skill** and pick `skills/summary-to-canvas.zip`.
3. Switch the skill on.

Update later: rebuild the zip and upload it again over the old one.

## Use it
<!-- meshfox:node id="use" -->
<!-- meshfox:edge from="install" -->

At the end of any conversation, say something like:

> Summarize this conversation as a meshfox canvas.

Claude replies with one `<slug>.canvas.md` file. Save it, then:

```bash
meshfox validate <slug>.canvas.md
meshfox view <slug>.canvas.md
```

If `validate` fails, paste its error back into the chat and ask Claude to fix the canvas.

## Notes
<!-- meshfox:node id="notes" color="3" -->

- If your plan doesn't offer Skills, paste `SKILL.md` and `example.canvas.md` into a Project's instructions/knowledge instead; it works the same way.
- The skill deliberately covers only the summary-shaped part of the format (nodes, ids, colors, tags, edges, `group`/`link`). The full spec is `meshfox spec`.
- Keep `example.canvas.md` valid — it is both the demo and what the build step validates.
