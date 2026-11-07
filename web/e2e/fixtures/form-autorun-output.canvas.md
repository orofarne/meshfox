<!-- meshfox:canvas -->
# Form Autorun Output Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright form-autorun-output suite
(`web/e2e/form-autorun-output.spec.ts`) — end-to-end coverage for the
`form`/`autorun`/`output="markdown"` combination (see SPEC.md's "Form
fences" and "Runnable code fences"): filling in a `form` fence's own
fields and clicking Send commits their values and automatically reruns
the `autorun` block that references them — no manual run, no reload —
and its freshly printed markdown table replaces the old one in place.

## Greeting
<!-- meshfox:node id="greeting" -->

Two node-scoped variables (implicitly `session` — see SPEC.md's
"Variables" — so neither ever touches the on-disk var cache, same
reasoning `vars-form.canvas.md`'s own fixture comment gives for its own
`session` variable: deterministic across repeated runs of this suite).

<!-- meshfox:var name="PERSON_NAME" prompt="Name" default="" -->
<!-- meshfox:var name="PERSON_CITY" prompt="City" default="" -->

```form name="greeting-form" send="Apply"
field var="PERSON_NAME" label="Name"
field var="PERSON_CITY" label="City"
```

```bash name="table" env="PERSON_NAME,PERSON_CITY" autorun output="markdown"
printf '| name | city |\n'
printf '|---|---|\n'
printf '| %s | %s |\n' "$PERSON_NAME" "$PERSON_CITY"
```

## Reset
<!-- meshfox:node id="resetting" -->

A form with the `reset` flag and a `keep` field (SPEC.md's "Form fences",
"Reset"): after a Send whose `autorun` block succeeds, the plain field goes
back to its default while the `keep` one stays; the Reset button clears both.

<!-- meshfox:var name="ITEM" prompt="Item" default="" -->
<!-- meshfox:var name="SHOP" prompt="Shop" default="corner" -->

```form name="reset-form" send="Add" reset
field var="ITEM" label="Item"
field var="SHOP" label="Shop" keep
```

```bash name="note" env="ITEM,SHOP" autorun output="markdown"
printf 'added %s at %s\n' "$ITEM" "$SHOP"
```
