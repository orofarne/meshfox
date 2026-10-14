<!-- meshfox:canvas -->
# Form Autorun Tty Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright form-autorun-tty suite
(`web/e2e/form-autorun-tty.spec.ts`) — a `form`'s Send reaching a `tty`
block carrying `autorun` (see SPEC.md's "Form fences" and "Interactive
(`tty`) blocks"): the server starts the terminal session on its own, the
open tab attaches to it without any click, and the form's Send stays
disabled for as long as that foreground run lasts.

## Session
<!-- meshfox:node id="session" -->

<!-- meshfox:var name="TTY_NAME" prompt="Name" default="" -->

```form name="session-form" send="Open"
field var="TTY_NAME" label="Name"
```

```bash name="shell" env="TTY_NAME" tty autorun
echo "hello $TTY_NAME"
sleep 4
echo "bye"
```
