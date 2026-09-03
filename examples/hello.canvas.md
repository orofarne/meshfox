<!-- meshfox:canvas -->
# Hello Project
<!-- meshfox:node id="root" -->

This is the project root. Everything else branches from here.

## Tests
<!-- meshfox:node id="tests" type="group" -->

### Smoke Test
<!-- meshfox:node id="smoke-test" -->

A trivial runnable check, with its output cached below the fence.

```bash name="smoke" cache
echo "hello from meshfox"
date
```
<!-- meshfox:output name="smoke" -->
```text
exit code: 0

hello from meshfox
Wed Aug 12 11:00:52 +04 2026
```
<!-- /meshfox:output -->

### Build Then Test
<!-- meshfox:node id="build-then-test" -->

Two blocks in the same node, chained with `deps`: running `test` always
runs `build` first, automatically — `deps="build"` is a bare name here
because `build` lives in *this* node.

```bash name="build" cache
echo "building..."
```
<!-- meshfox:output name="build" -->
```text
exit code: 0

building...
```
<!-- /meshfox:output -->

```bash name="test" cache deps="build"
echo "testing..."
```
<!-- meshfox:output name="test" -->
```text
exit code: 0

testing...
```
<!-- /meshfox:output -->

## Examples
<!-- meshfox:node id="examples" type="group" -->

### Shared Smoke Check
<!-- meshfox:node id="shared-smoke" -->
<!-- meshfox:edge from="tests" -->

Reused from Tests as well — this node has two parents: Examples (its
lexical parent) and Tests (declared via the `meshfox:edge` line above).

### Deploy (cross-node dependency)
<!-- meshfox:node id="deploy-example" -->

`deps` can also point at a block in a *different* node, as
`node-id/block-name` — here, `build-then-test/test`. Running `deploy`
runs `build`, then `test`, then `deploy`, in that order.

```bash name="deploy" cache deps="build-then-test/test"
echo "deploying..."
```
<!-- meshfox:output name="deploy" -->
```text
exit code: 0

deploying...
```
<!-- /meshfox:output -->

`verify` closes out the chain but has no `cache` flag — it still runs
(and still runs `deploy` and its own deps first), it just never writes
an output block back into the file, so nothing below appears here even
after running it.

```bash name="verify" deps="deploy"
echo "verifying..."
```

### Release (multiple dependencies)
<!-- meshfox:node id="release-example" -->

`deps` is a comma-separated list — a block can depend on more than one
other block at once, mixing same-node and cross-node references. Here
`release` depends on both `build-then-test/test` and
`deploy-example/deploy`; since `deploy` itself already depends on
`test` (which depends on `build`), running `release` still only runs
`build` and `test` once each — shared dependencies aren't re-run just
because more than one thing needs them.

```bash name="release" cache deps="build-then-test/test,deploy-example/deploy"
echo "releasing..."
```
<!-- meshfox:output name="release" -->
```text
exit code: 0

releasing...
```
<!-- /meshfox:output -->

### Lint (implicit block name)
<!-- meshfox:node id="lint" -->

A node with just one fence and no `name=` at all is runnable too,
implicitly named after its own node id (`lint`) — so it's addressed as
`meshfox run examples lint`, no separate trailing block name needed
(dropping a name that would just repeat `lint` a second time). A second
fence in this node, named or not, would make that ambiguous and turn the
shortcut off.

```bash cache
echo "linting..."
```
<!-- meshfox:output name="lint" -->
```text
exit code: 0

linting...
```
<!-- /meshfox:output -->

## Links
<!-- meshfox:node id="links" type="group" -->

### Project Homepage
<!-- meshfox:node id="homepage" type="link" preview="true" -->

[Project Homepage](https://github.com/orofarne/meshfox)

### Architecture Diagram
<!-- meshfox:node id="architecture-diagram" type="file" -->

[architecture](./architecture.png)

### Related Canvas
<!-- meshfox:node id="related-canvas" type="file" -->

[vars.canvas.md](./vars.canvas.md)

