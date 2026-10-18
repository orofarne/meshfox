<!-- meshfox:canvas -->
# Tangle Demo
<!-- meshfox:node id="root" -->

A fence with `interpreter=` normally runs from a throwaway file in the system
temp directory. That is fine for self-contained code, but anything that
resolves a path **relative to the script file** — Ruby's `require_relative`,
Node's `import "./helper.mjs"`, a nearby `package.json` — looks in the wrong
place. Adding the `tangle` flag makes meshfox write the block's code to a
stable hidden file next to where the block runs instead
(`.meshfox-tangle-<canvas file>__<block name>.<ext>`), and run that.

(Bare package imports are a different matter — see
[node-env.canvas.md](./node-env.canvas.md) and
[python-venv.canvas.md](./python-venv.canvas.md).)

## Where the code lives
<!-- meshfox:node id="where" -->

The same one-liner twice. Without `tangle` the script is a temp file; with it
the script is a hidden file beside this canvas, and its name is the same on
every run.

```ruby name="where-plain" interpreter="ruby"
puts __FILE__
```

```ruby name="where-tangled" interpreter="ruby" tangle
puts __FILE__
```

## A file next to the canvas
<!-- meshfox:node id="require-relative" -->

[tangle_helper.rb](./tangle_helper.rb) sits beside this canvas. `slug` below
loads it with `require_relative`, which Ruby resolves from the script's own
directory — so it only works because the script is in this directory.

```ruby name="slug" interpreter="ruby" tangle
require_relative "tangle_helper"

puts TangleHelper.slugify("Hello, Tangled World!")
```

Drop `tangle` from that fence and it fails with `cannot load such file` — the
temp copy has no `tangle_helper.rb` beside it.

## Things to know
<!-- meshfox:node id="notes" -->

 - The hidden files are a cache of this canvas, never a source: add
   `.meshfox-tangle-*` to your `.gitignore`. Delete a block (or the flag) and
   the worker removes its file on the next start; `meshfox prune` clears out
   leftovers of renamed or deleted canvases.
 - Code is rewritten only when it changes, so re-running the same block does
   not wake file watchers (nodemon, Vite, …) — but they do not ignore dotfiles
   by default, so a block that edits its own code will trigger them.
 - The script now lives in the project, so the nearest `package.json` and
   `tsconfig.json` apply to it. A `"type": "module"` makes a Node block ESM.
 - If the directory is read-only the run fails with an explicit error rather
   than quietly falling back to `/tmp`.
