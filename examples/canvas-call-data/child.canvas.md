<!-- meshfox:canvas -->
# The child canvas
<!-- meshfox:node id="root" -->

This canvas is called by [the parent example](../canvas-call.canvas.md). `WHO` is a child document variable; it enters the process only through the block's `env` attribute.

<!-- meshfox:var name="WHO" type="string" prompt="Who should the child greet?" -->
<!-- meshfox:var name="INPUT" type="string" prompt="What should the child print?" -->

Run the child directly to compare its output with the parent's nested output:

```console
meshfox examples/canvas-call-data/child.canvas.md run greet --set WHO=direct
```

```bash name="greet" env="$WHO"
printf 'Hello, %s!\n' "$WHO"
printf 'Child working directory: %s\n' "$PWD"
sleep 0.4
```

```bash name="slow"
printf 'Child started: cancellation should stop this run.\n'
sleep 15
printf 'Child completed.\n'
```

## Ask
<!-- meshfox:node id="ask" -->

The parent can collect this application's input in its own UI, while the value remains owned by this child canvas.

```bash name="ask" env="$INPUT"
printf 'Child received: %s\n' "$INPUT"
sleep 0.4
```

## Fail
<!-- meshfox:node id="fail" -->

An intentional failure for checking propagation to the parent.

```bash name="fail"
printf 'Intentional child failure.\n' >&2
exit 7
```

