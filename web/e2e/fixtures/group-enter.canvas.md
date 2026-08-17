<!-- meshfox:canvas -->
# Group Enter Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright group-enter suite
(`web/e2e/group-enter.spec.ts`). `frame` is a group with two direct
members; `outsider` is a plain sibling, used to check the mini sub-canvas
shows only the group's own members, not everything on the page. Declares
`unfold` (see SPEC.md's "Options") so `frame` (which, having children,
would otherwise start folded to a compact title-only row by default)
renders at its normal auto-laid-out size, matching this suite's own
coordinate/overlap assumptions.

One of this suite's tests drags a member inside the mini canvas to check
it persists — like `group-drag.canvas.md`, a real run genuinely rewrites
this file's `x`/`y` in place. Harmless in CI (a fresh checkout every
time); `git checkout` it back afterward if running locally leaves it
modified and that matters to you.

## Frame
<!-- meshfox:node id="frame" type="group" x=400 y=400 -->

### Member Two
<!-- meshfox:node id="member-two" x=20 y=160 w=200 h=100 -->

Second member.

### Member One
<!-- meshfox:node id="member-one" x=336 y=208 w=200 h=100 -->

First member.

## Outsider
<!-- meshfox:node id="outsider" -->

A plain sibling of Frame, not one of its members.
