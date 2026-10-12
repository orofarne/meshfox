<!-- meshfox:canvas -->
# Path Picker Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright path-picker suite
(`web/e2e/path-picker.spec.ts`): a `form` fence whose fields are a
`type="file"` input (with an `ext=` hint), a `mode="save"` output and a
`type="dir"`, each getting a path field with a "Browse…" button that opens
the worker-backed file browser (`GET /api/fs/list`). The files it browses
live in `path-picker-data/` next to this canvas. The variables are `session`
so no answer ever lands in the on-disk var cache between runs.

<!-- meshfox:var name="INPUT" type="file" ext="mp4,mkv" prompt="Input video" session -->
<!-- meshfox:var name="OUTPUT" type="file" mode="save" prompt="Output file" session -->
<!-- meshfox:var name="WORKDIR" type="dir" prompt="Work directory" session -->

```form name="pick" send="Apply"
field var="INPUT" label="Input video"
field var="OUTPUT" label="Output file"
field var="WORKDIR" label="Work directory"
```
