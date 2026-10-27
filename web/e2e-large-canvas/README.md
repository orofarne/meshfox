# Large canvas regression

Run from the repository root:

```sh
npm --prefix web run test:e2e:large-canvas
```

The pretest builds the embedded production UI and CLI and signs the CLI on
macOS. The suite starts its own direct worker on 4769 and gateway on 4768,
with separate temporary copies of the fixture, scratch HOME and an isolated
coordinator socket. Execution is disabled. It does not use Docker, the
original document or the developer's daemon.

`fixtures/synthetic.canvas.md` contains only test data: one root, 48 branches
and 511 leaves (560 text nodes), without authored positions or sizes. It was
created through the canvas worker API. There are no Sonar contents, snapshots
or mocked API responses. This synthetic tree reproduced React #185 with the
unfixed production UI (index-DwqW4Ikw.js).

Firefox and Chromium run the same scenarios at `/` and `/c/<slug>/`, with
default folding and with all 560 nodes expanded. A passing case requires
visible measured nodes, ten seconds without browser errors and the same
checks after reload. Expanded cases also require all 560 DOM nodes and 559 incoming arrows, and
collapse/reopen the root to exercise bulk remounting. Default-fold cases
require 49 visible nodes and 48 incoming arrows.

Each case writes browser-errors.json. Failures retain screenshots and
Playwright traces. The JSON report is test-results/large-canvas-summary.json.

For one comparison:

```sh
cd web
npx playwright test -c playwright.large-canvas.config.ts --project=firefox-direct --grep expanded
npx playwright test -c playwright.large-canvas.config.ts --project=firefox-gateway --grep expanded
```
