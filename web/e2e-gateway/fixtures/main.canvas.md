<!-- meshfox:canvas -->
# Gateway fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture for the gateway suite (`web/e2e-gateway/gateway.spec.ts`): the same
page the other suites drive, but reached through `meshfox serve --http` under
`/c/<slug>/`.

<!-- meshfox:var name="PICKED" type="file" prompt="Pick a file" session -->

```form name="pick" send="Apply"
field var="PICKED" label="Pick a file"
```

## Run
<!-- meshfox:node id="run" parent="root" -->

```bash name="greet"
echo "hello from the gateway"
```

## Code
<!-- meshfox:node id="code" parent="root" -->

A fence in a language whose grammar is a lazy chunk (not part of the main bundle):

```cpp
int main() { return 0; }
```

## Other canvas
<!-- meshfox:node id="other" type="file" parent="root" -->

[other](./sub/other.canvas.md#child)

## A text file
<!-- meshfox:node id="note" type="file" parent="root" -->

[note](./note.txt)
