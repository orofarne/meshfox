<!-- meshfox:canvas -->
# Interpreters Demo
<!-- meshfox:node id="root" -->

Every shape of the `interpreter=` attribute, side by side — see SPEC.md's
"Runnable code fences" section (the `interpreter=` entry) for the full
reference. The cached output beneath each runnable block below is from
an actual `meshfox run` invocation, not typed by hand — e.g. `meshfox
run --canvas examples/interpreters.canvas.md python-example seed`. Each
block below is also flagged `default`, so the web UI's own node-level
"▷ run" (in the title bar, next to "expand") runs it too — not just the
block's own Run button inside the node body.

## Python
<!-- meshfox:node id="python-example" -->

A fenced block's `lang` is only ever a syntax-highlighting hint once its
own `interpreter=` is set — this fence's `lang` (`python`) never has to
be `bash`/`sh` for the block to count as runnable, unlike a plain fence
with no `interpreter=` at all.

```python name="seed" interpreter="python3" cache default
print("hello from python, seeded")
```
<!-- meshfox:output name="seed" -->
```text
exit code: 0

hello from python, seeded
```
<!-- /meshfox:output -->

## Command with flags
<!-- meshfox:node id="flags-example" -->

`interpreter=` is word-split the same way a real shebang line's own
arguments would be (`#!/usr/bin/env -S ...`), quoting included:
`interpreter="python3 -u"` runs `python3` with `-u` (unbuffered stdout)
in front of the block's own temp file — the command-plus-flags shape,
not just a bare executable name.

```python name="unbuffered" interpreter="python3 -u" cache default
print("this prints immediately, unbuffered")
```
<!-- meshfox:output name="unbuffered" -->
```text
exit code: 0

this prints immediately, unbuffered
```
<!-- /meshfox:output -->

## Any installed interpreter
<!-- meshfox:node id="node-example" -->

Not just Python — anything on `PATH` works, since `interpreter=` is just
a command to run the fence's own body against.

```javascript name="hello-node" interpreter="node" cache default
console.log("hello from node " + process.version);
```
<!-- meshfox:output name="hello-node" -->
```text
exit code: 0

hello from node v26.7.0
```
<!-- /meshfox:output -->

## Interactive (`tty` + `interpreter`)
<!-- meshfox:node id="tty-example" -->

`interpreter=` works on `tty` blocks too — the block hands its real
terminal over to `interpreter` directly instead of the implicit `bash`.
Not run here — `tty` and `cache` are mutually exclusive (see SPEC.md's
"Runnable code fences"), and this needs a real interactive terminal to
actually use — but it's a valid, runnable block:
`meshfox run --canvas examples/interpreters.canvas.md tty-example repl`,
from an interactive shell, drops straight into a real Python REPL.

```python name="repl" interpreter="python3 -i" tty default
print("welcome — this is a real interactive Python REPL")
```

## File-node counterpart
<!-- meshfox:node id="file-example" -->

The same `interpreter=` mechanism, just on a `file`-type node instead of
a fence — this is actually where it originated (see SPEC.md's "Node
types"); a runnable fence's own `interpreter=` is the generalized,
fenced-block counterpart of this.

### Seed script
<!-- meshfox:node id="seed-script" type="file" interpreter="python3" -->

[seed.py](interpreters-data/seed.py)
