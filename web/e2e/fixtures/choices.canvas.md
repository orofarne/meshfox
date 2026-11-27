<!-- meshfox:canvas -->
# Choices Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright choices suite (`web/e2e/choices.spec.ts`,
`choices-no-exec.spec.ts`): how a `select` fed by a computed list
(`choices_var=` on a `from=` variable) is offered — a filterable list — and
what a field says when the list could not be computed. Kept apart from
`vars-form.canvas.md` so this extra height does not push that canvas's blocks
out of the fit-view zoom the arguments suite relies on.

<!-- meshfox:var name="FONT_LIST" from="fonts/list-fonts" -->
<!-- meshfox:var name="BROKEN_LIST" from="broken/list-broken" -->
<!-- meshfox:var name="CFG_FONT" prompt="Configured font?" type="select" choices_var="BROKEN_LIST" -->

## Fonts
<!-- meshfox:node id="fonts" -->

A `select` with a long list of computed choices: the control filters it as
you type (the same rule as the TUI picker and the `meshfox` prompt).

<!-- meshfox:var name="FONT" prompt="Font?" type="select" choices_var="FONT_LIST" required session -->

```bash name="list-fonts"
echo "FONT_LIST=Arial,Arial Black,Courier New,DejaVu Sans,DejaVu Sans Mono,Fira Code,Georgia,Helvetica,Helvetica Neue,Menlo,Monaco,Noto Mono,Noto Sans,Noto Serif,Times New Roman,Verdana" >> "$MESHFOX_VARS_OUT"
```

```bash name="use-font" env="$FONT"
echo "font is $FONT"
```


## Broken
<!-- meshfox:node id="broken" -->

The source of a `select`'s choices fails: the control must say so (and
why) instead of showing an empty list.

<!-- meshfox:var name="BROKEN_FONT" prompt="Broken font?" type="select" choices_var="BROKEN_LIST" required session -->

```bash name="list-broken"
echo "no fontconfig here" >&2
exit 3
```

```bash name="use-broken" env="$BROKEN_FONT"
echo "font is $BROKEN_FONT"
```

```form name="pick-broken" send="Pick"
field var="BROKEN_FONT"
```


## Hosts
<!-- meshfox:node id="hosts" -->

A `string` with `suggest_var=` (computed list) and one with a literal
`suggest=`: the control is a text input that offers candidates while typing,
and any other text is still a valid answer.

<!-- meshfox:var name="HOST_LIST" from="hosts/list-hosts" -->
<!-- meshfox:var name="HOST" prompt="Host?" suggest_var="HOST_LIST" required session -->
<!-- meshfox:var name="STAGE" prompt="Stage?" suggest="dev,staging,prod" required session -->

```bash name="list-hosts"
echo "HOST_LIST=web-1.example,web-2.example,db-1.example" >> "$MESHFOX_VARS_OUT"
```

```bash name="connect" env="$HOST,$STAGE"
echo "connecting to $HOST ($STAGE)"
```
