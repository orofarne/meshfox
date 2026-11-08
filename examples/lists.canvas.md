<!-- meshfox:canvas -->
# Lists
<!-- meshfox:node id="root" -->

A `type="list"` variable holds an ordered list of strings. It has no separator
of its own: the block that **writes** a list picks how it writes it, and each
block that **reads** it picks how it looks (`env="NAME|format"`). A deps entry
`name=*$LIST` runs the dependency once per element. See SPEC.md's "List
variables". Everything below needs only `bash`.

<!-- meshfox:var name="NUMBERS" type="list" of="int" from="map/numbers" -->
<!-- meshfox:var name="REGIONS" type="list" choices="us,eu,asia" default='["eu"]' -->
<!-- meshfox:var name="ARCHES" type="list" default='["amd64","arm64"]' -->

## Map and reduce
<!-- meshfox:node id="map" parent="root" -->

A producer lists the work (one `NUMBERS[]=` line per element, so any text is
fine), a parameterized block handles one element, and the consumer's `deps=`
spreads over the list. Run `meshfox examples/lists.canvas.md run map total`:
`numbers` runs first, then `square[n=3]`, `square[n=1]`, `square[n=2]` in list
order, then `total`, which also receives the list itself to reduce over.

```bash name="numbers"
for n in 3 1 2; do echo "NUMBERS[]=$n" >> "$MESHFOX_VARS_OUT"; done
```

<!-- meshfox:arg name="n" type="int" -->
```bash name="square"
echo "$n squared is $((n * n))"
```

```bash name="total" deps='square[n=*$NUMBERS]' env="NUMBERS|space"
sum=0
for n in $NUMBERS; do sum=$((sum + n * n)); done
echo "sum of squares of $NUMBERS = $sum"
```

A list that comes out empty runs no `square` at all, and `total` still runs: the
dependency simply disappears.

## One list, three readers
<!-- meshfox:node id="formats" parent="root" -->

The same list reaches each block in the format that block asked for.
`json` (the default) is lossless; `space` is shell-quoted words;
`pg` is a PostgreSQL array literal, as a `psql` block would want it.

```bash name="as-shell" deps="map/numbers" env="NUMBERS|space"
echo "shell words: $NUMBERS"
```

```bash name="as-postgres" deps="map/numbers" env="IDS=NUMBERS|pg"
echo "select * from t where id = any('$IDS');"
```

```bash name="as-json" deps="map/numbers" env="NUMBERS"
echo "json: $NUMBERS"
```

## Choose from a list, then fan out
<!-- meshfox:node id="rollout" parent="root" -->

A list with `choices` is a set of checkboxes in the web UI and a numbered menu
at the terminal (`1, 3` or names). The form is a step of the chain, so the run
waits for the answer; the answer then fans out into one application per region
and architecture — the cartesian product.

```form name="pick" send="Roll out"
text "Which regions?"
field var="REGIONS" label="Regions"
```

<!-- meshfox:arg name="region" -->
<!-- meshfox:arg name="arch" -->
```bash name="deploy"
echo "deploying to $region on $arch"
```

```bash name="rollout" deps='pick,deploy[region=*$REGIONS,arch=*$ARCHES]'
echo "rolled out"
```
