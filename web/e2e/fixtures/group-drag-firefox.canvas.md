<!-- meshfox:canvas -->
# Group Drag Fixture
<!-- meshfox:node id="root" -->

Fixture canvas for the Playwright group-drag persistence suite
(`web/e2e/group-drag.spec.ts`). `frame` is a group with a real anchor of
its own; `member-one`/`member-two` are its two real, group-relative
members — this is what lets a drag on either the group's own title bar or
on one member in isolation be told apart in the saved file.

Unlike this suite's siblings (which stay read-only, or explicitly assert
"ok" never writes), this one's whole point is drag-and-persist — a real
run genuinely rewrites this file's `x`/`y` values in place. Harmless in
CI (a fresh checkout every time), but running it locally leaves this
fixture modified on disk; `git checkout` it back afterward if that
matters to you.

An exact duplicate of `group-drag.canvas.md`, on its own server/port
(`playwright.config.ts`'s `GROUP_DRAG_FIREFOX_PORT`) for the `firefox-
group-drag` project specifically — this suite genuinely rewrites its
fixture on disk, and the `chrome-group-drag`/`firefox-group-drag`
projects otherwise shared one server/file, so a real drag-and-persist
write from one browser's run could race a concurrent read/write from the
other's when both ran in the same `playwright test` invocation.

## Frame
<!-- meshfox:node id="frame" type="group" x=2080 y=1144 -->

### Member Two
<!-- meshfox:node id="member-two" x=20 y=160 w=200 h=100 -->

Second member.
### Member One
<!-- meshfox:node id="member-one" x=240 y=20 w=200 h=100 -->

First member.
