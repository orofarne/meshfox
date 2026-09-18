<!-- meshfox:canvas -->
# Before External Edit
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for `web/e2e/external-edit.spec.ts` — regression coverage for
the on-disk file watcher (`spawn_file_watcher`, `crates/server/src/lib.rs`)
picking up a plain text edit made straight to this file (no MCP, no CLI, no
`/api/*` write) and pushing it to an already-open tab over `/api/watch`,
without the test ever calling `page.reload()`. One bare root, titled
distinctively so the test can assert on it changing in place.

Body marker: BEFORE-BODY-EDIT.
