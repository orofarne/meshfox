<!-- meshfox:canvas -->
# Vars Form Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright vars-form suite (`web/e2e/vars-form.spec.ts`)
— regression coverage for a bug where the "Configure variables" modal's
`<select>` rendered with zero options for a `choices_var`-declared
variable whose reference chain reaches a `from=`-computed one
(`REGIONS_LIST`, computed by `list-regions` below): `GET /api/vars` never
executed anything, so the choices could never be known ahead of a real
run, unlike the CLI/TUI (which resolve lazily mid-chain and so had
already run `list-regions` by the time the choices were needed). See
`crates/server/src/lib.rs`'s `materialize_choices_and_defaults`.
`REGION` is `session` so the modal reappears deterministically on every
test run (including across browser projects sharing this same server) —
without it, a first run's answer would persist to the on-disk var cache
and later runs would silently skip the modal entirely.

<!-- meshfox:var name="REGIONS_LIST" from="dynamic/list-regions" -->
<!-- meshfox:var name="REGION" prompt="Region?" type="select" choices_var="REGIONS_LIST" required session -->

## Dynamic
<!-- meshfox:node id="dynamic" -->

```bash name="list-regions"
echo "REGIONS_LIST=us-east-1,eu-west-1" >> "$MESHFOX_VARS_OUT"
```

```bash name="use-region" env="$REGION"
echo "using $REGION"
```


## Clips
<!-- meshfox:node id="clips" -->

A `type="file"` variable: the run-gate modal shows it as a path field with a
"Browse…" button (the worker-backed file browser, see `path-picker.canvas.md`).
`path-picker-data/` is a sibling of this canvas in the copied fixtures dir.

<!-- meshfox:var name="CLIP" type="file" ext="mp4,mkv" prompt="Clip?" required session -->

```bash name="use-clip" env="$CLIP"
echo "clip is $CLIP"
```


## Report
<!-- meshfox:node id="report" -->

A block that writes a file the node below previews with `display="code"` —
the preview has to re-read it when the run ends, not only when the node
first mounts (`FileRefreshContext` in `web/src/MeshNode.tsx`).

```bash name="write-report"
echo "report-$(date +%s%N)" > report.txt
```

### Report file
<!-- meshfox:node id="report-file" type="file" display="code" -->

[report.txt](report.txt)
