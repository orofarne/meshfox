<!-- meshfox:canvas -->
# Node Env Demo
<!-- meshfox:node id="root" -->

A canvas-local `node_modules` for Node, following the same `meshfox:var` `from=`
pattern as [python-venv.canvas.md](./python-venv.canvas.md): the `node-setup`
block below installs this demo's one dependency into
`.meshfox/node-env.canvas.md.node/` (once — it is skipped until the declared
packages change) and reports a launcher for `node` as the computed `NODE`
variable. Every Node-running fence references it via `interpreter="$NODE"`
instead of needing an `npm install` next to the canvas, so the document
carries everything it needs.

That `interpreter=` reference is also the only ordering this canvas needs: a
`from=`-declared variable's source block is an implicit dependency of anything
that resolves it, so `render` below has no `deps=` of its own.

<!-- meshfox:var name="NODE" from="environment/node-setup" -->

## Environment setup
<!-- meshfox:node id="environment" -->

`interpreter="@node_env"` is a built-in interpreter, like `@python_venv`. A
`text` fence lists npm packages, one per line (`pele`, `left-pad@1.3.0`,
`@scope/pkg@^2`; blank lines and `#` comments are skipped); a `json` fence is
a whole `package.json` instead. The launcher it reports runs the `node` on
your `PATH` with this environment's packages visible to both `require` and
`import` (Node 20.6 or newer). Relative imports (`./helper.js`) are a
different matter — those resolve from where the code's file lives, which is
what a block's `tangle` flag is for.

```text name="node-setup" interpreter="@node_env" cache
pele@1.6.0
```

## Rendering a Mermaid diagram
<!-- meshfox:node id="render-node" -->

[Pele](https://pele.run) is a small pure-JS Mermaid renderer — no browser, a few
milliseconds per diagram. It is young and draws with its own layout and style,
so a diagram can look different from `mmdc`'s. The block prints the SVG and
`output="image"` shows it as a picture (see [svg.canvas.md](./svg.canvas.md)).

```javascript name="render" interpreter="$NODE" cache output="image" output-attrs="bg=#fff"
import { render } from "pele";

const { svg } = render(`flowchart LR
  A[Block] -->|prints SVG| B(output=image) --> C{{Picture}}`);
process.stdout.write(svg);
```
