<!-- meshfox:canvas -->
# Copy Paste Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright real-clipboard copy/paste suite
(`web/e2e/copy-paste.spec.ts` — TODO.canvas.md: "VSCode: вставка текста
(Cmd+V и контекстное меню) в редактор ноды не работает"). One bare root is
enough: `MONACO_OPTIONS` is shared between the node body editor
(NodeTextEditor) and the whole-document source editor (CanvasSourceEditor),
and this fixture's root is a target for both.

Unlike this suite's siblings that only read, this one genuinely rewrites
this file's root body on disk (NodeTextEditor auto-saves on close) — a
real local run leaves it modified; `git checkout` it back afterward if
that matters to you. `copy-paste-firefox.canvas.md` is an exact duplicate
of this file on its own server/port (`playwright.config.ts`'s
`COPY_PASTE_FIREFOX_PORT`) for the `firefox-copy-paste` project
specifically — this suite's round-trip test genuinely runs on both
browsers (unlike image-paste.spec.ts, which skips Firefox entirely), so a
real write from one browser's run could otherwise race a concurrent
read/write from the other's when both ran in the same `playwright test`
invocation.
