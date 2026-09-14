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
