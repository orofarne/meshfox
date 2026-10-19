<!-- meshfox:canvas -->
# TUI screencast

Record a silent TUI demo, roughly 30 seconds long, as MP4 and WebM, with sharp SIXEL images.

Run **Install dependencies** once and **Build meshfox** when needed, then **Record with Playwright**. Playwright and its Chromium browser are prepared through the canvas-local **Node environment** automatically. Blocks run relative to this document (`scripts/screencast`).

<!-- meshfox:var name="output_dir" type="dir" default="../../target/screencast-playwright" prompt="Output directory" -->
<!-- meshfox:var name="NODE" from="node-environment/node-setup" -->

The demo shows Markdown and tree navigation, live canvas search, streamed ANSI output, changing a variable through Configure, rerunning, a local chart image, and saving a prose edit in the source editor.

Timing and keystrokes live in [demo.tape](demo.tape). ttyd renders the TUI and SIXEL layer; Playwright records the full browser viewport. The report uses only Bash, printf and sleep; the chart is a bundled PNG.

## Install dependencies
<!-- meshfox:node id="install-dependencies" -->

Install ttyd, ffmpeg and Node.js with Homebrew on macOS. Playwright is managed separately by the canvas-local Node environment.

```bash name="install"
brew install ttyd ffmpeg node
```

## Build meshfox
<!-- meshfox:node id="build-meshfox" -->

Build and install meshfox through the repository's canonical installation step. The recorder uses the installed `meshfox` from `PATH`.

```meshfox name="build"
canvas: ../../README.md
run: install-from-source
```

This native call runs in the target canvas's worker and streams its steps and output here.

## Record with Playwright
<!-- meshfox:node id="record-with-playwright" -->

Record the TUI through ttyd with SIXEL and full-browser Playwright capture. Playwright comes from this document's own Node environment. The Chromium dependency is installed automatically when needed.

The canvas directory variable `output_dir` selects the output directory. Confirm its suggested value once in Configure; meshfox then trusts this directory for opening the result file nodes. Outputs: `meshfox-tui-sixel.mp4`, `meshfox-tui-sixel.webm`, and `sixel-chart.png`.

```javascript name="record-playwright" interpreter="$NODE" env="$output_dir" deps="node-environment/browser-install"
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { spawn, execFileSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { chromium } from 'playwright';
const demoDir = process.cwd();
const outputDir = path.resolve(process.env.output_dir);
const binary = process.env.MESHFOX_DEMO_BIN || execFileSync('which', ['meshfox'], { encoding: 'utf8' }).trim();
const work = await fs.mkdtemp(path.join(os.tmpdir(), 'meshfox-playwright-'));
let ttyd, browser, context;
let ttyLog = '';
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
try {
  await fs.mkdir(outputDir, { recursive: true });
  for (const name of ['demo.canvas.md', 'sales.png']) {
    await fs.copyFile(path.join(demoDir, name), path.join(work, name));
  }
  await fs.mkdir(path.join(work, '.meshfox'));
  await fs.writeFile(path.join(work, '.meshfox/config.toml'),
    'server_socket = ""\n[tui]\neditor_theme = "base16-ocean.dark"\n');
  ttyd = spawn('ttyd', ['-i', '127.0.0.1', '-p', '0', '-W', '-o',
    '-t', 'enableSixel=true', '-t', 'fontSize=22', '-t', 'rendererType=canvas',
    '-t', 'disableResizeOverlay=true', binary, 'tui', 'demo.canvas.md'], {
    cwd: work,
    env: { ...process.env, MESHFOX_SERVER_SOCKET: '', MESHFOX_IMAGE_PROTOCOL: 'sixel',
      PATH: path.dirname(binary) + path.delimiter + process.env.PATH },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  ttyd.on('error', error => { ttyLog += error.message; });
  ttyd.stdout.on('data', chunk => { ttyLog += chunk; });
  ttyd.stderr.on('data', chunk => { ttyLog += chunk; });
  const deadline = performance.now() + 10000;
  while (!/Listening on port:\s*(\d+)/.test(ttyLog)) {
    if (ttyd.exitCode !== null || performance.now() > deadline) throw Error(ttyLog || 'ttyd startup timed out');
    await sleep(50);
  }
  const port = ttyLog.match(/Listening on port:\s*(\d+)/)[1];
  browser = await chromium.launch();
  context = await browser.newContext({
    viewport: { width: 1600, height: 1000 },
    recordVideo: { dir: path.join(outputDir, 'raw'), size: { width: 1600, height: 1000 } },
  });
  const page = await context.newPage();
  page.on('pageerror', error => console.error('Browser:', error.message));
  await page.goto('http://127.0.0.1:' + port);
  await page.waitForFunction(() => window.term?.cols > 0);
  await page.locator('.xterm-helper-textarea').focus();
  // Reuse the existing tape's interactions; ttyd launches the TUI directly.
  const tape = (await fs.readFile(path.join(demoDir, 'demo.tape'), 'utf8')).split('\n');
  const firstWait = tape.findIndex(line => line.startsWith('Wait+Screen'));
  let chartPending = false;
  let chartCaptured = false;
  for (const raw of tape.slice(firstWait)) {
    const line = raw.trim();
    if (!line || line.startsWith('#') || line === 'Show' || line === 'Hide') continue;
    console.log(line);
    if (line.startsWith('Wait+Screen ')) {
      const source = line.slice(line.indexOf('/') + 1, line.lastIndexOf('/'));
      await page.waitForFunction(source => {
        const buffer = window.term.buffer.active;
        const rows = [];
        for (let i = buffer.viewportY; i < buffer.viewportY + window.term.rows; i++)
          rows.push(buffer.getLine(i)?.translateToString(true) || '');
        return new RegExp(source).test(rows.join('\n'));
      }, source);
      if (source.includes('Images render')) chartPending = true;
    } else if (line.startsWith('Sleep ')) {
      const value = line.slice(6);
      await sleep(parseFloat(value) * (value.endsWith('ms') ? 1 : 1000));
      if (chartPending && !chartCaptured) {
        await page.waitForFunction(() => {
          const canvas = document.querySelector('canvas.xterm-image-layer');
          if (!canvas) return false;
          const rgba = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
          let visible = 0;
          for (let i = 3; i < rgba.length; i += 4) if (rgba[i]) visible++;
          return visible > 1000;
        });
        await page.screenshot({ path: path.join(outputDir, 'sixel-chart.png') });
        await fs.writeFile(path.join(outputDir, 'chart-time.json'),
          JSON.stringify({ seconds: await page.evaluate(() => performance.now() / 1000) }));
        chartCaptured = true;
      }
    } else if (line.startsWith('EndCard ')) {
      const logo = await fs.readFile(path.resolve(demoDir, '../../web/public/icon-512.png'));
      await page.evaluate(async logo => {
        const card = document.createElement('div');
        card.style.cssText = 'position:fixed;inset:0;z-index:2147483647;background:#0b0b0b;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:24px;color:#f4f4f4;font:600 60px system-ui,sans-serif';
        const image = document.createElement('img');
        image.src = logo;
        image.width = 320;
        image.height = 320;
        image.alt = 'Meshfox';
        await image.decode();
        const title = document.createElement('div');
        title.textContent = 'Meshfox';
        card.append(image, title);
        document.body.append(card);
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      }, 'data:image/png;base64,' + logo.toString('base64'));
      await page.screenshot({ path: path.join(outputDir, 'end-card.png') });
      await sleep(parseFloat(line.slice(8)) * 1000);
    } else if (line.startsWith('Type ')) {
      await page.keyboard.type(line.slice(6, -1), { delay: 40 });
    } else if (line.startsWith('Backspace ')) {
      for (let i = 0; i < Number(line.slice(10)); i++) await page.keyboard.press('Backspace');
    } else if (['Enter', 'Escape', 'Ctrl+S'].includes(line)) {
      await page.keyboard.press(line === 'Ctrl+S' ? 'Control+s' : line);
    } else {
      throw Error('Unsupported tape command: ' + line);
    }
  }
  if (!chartCaptured) throw Error('Chart was never captured');
  const video = page.video();
  await context.close();
  context = undefined;
  const videoPath = await video.path();
  const webm = path.join(outputDir, 'meshfox-tui-sixel.webm');
  await fs.copyFile(videoPath, webm);
  execFileSync('ffmpeg', ['-y', '-i', webm, '-c:v', 'libx264', '-pix_fmt', 'yuv420p',
    '-movflags', '+faststart', path.join(outputDir, 'meshfox-tui-sixel.mp4')], { stdio: ['ignore', 'ignore', 'pipe'] });
  console.log('Created SIXEL WebM, MP4 and screenshot in ' + outputDir);
} finally {
  await context?.close().catch(() => {});
  await browser?.close();
  if (ttyd && ttyd.exitCode === null) {
    ttyd.kill('SIGTERM');
    await Promise.race([new Promise(resolve => ttyd.once('exit', resolve)), sleep(2000)]);
    if (ttyd.exitCode === null) ttyd.kill('SIGKILL');
  }
  await fs.rm(work, { recursive: true, force: true });
}
```

From the repository root:

```text
meshfox scripts/screencast/README.canvas.md run --set output_dir=/tmp/meshfox-video record-with-playwright record-playwright
```

### Node environment
<!-- meshfox:node id="node-environment" -->

Playwright is installed in this canvas's own `.meshfox/README.canvas.md.node/` environment by the built-in `@node_env` interpreter. Node.js and npm must be available on `PATH`.

```text name="node-setup" interpreter="@node_env"
playwright@1.64.0
```

Install the matching Chromium browser. The recording block depends on this step; already installed browser binaries are reused.

```javascript name="browser-install" interpreter="$NODE"
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
const packageFile = fileURLToPath(import.meta.resolve('playwright/package.json'));
const cli = path.join(path.dirname(packageFile), 'cli.js');
execFileSync(process.execPath, [cli, 'install', 'chromium'], { stdio: 'inherit' });
```

### Recording tape
<!-- meshfox:node id="recording-tape" type="file" -->

[demo.tape](demo.tape)

### Video MP4
<!-- meshfox:node id="video-mp4" type="file" -->

[meshfox-tui-sixel.mp4]($output_dir/meshfox-tui-sixel.mp4)

### Video WebM
<!-- meshfox:node id="video-webm" type="file" -->

[meshfox-tui-sixel.webm]($output_dir/meshfox-tui-sixel.webm)

### SIXEL chart screenshot
<!-- meshfox:node id="sixel-chart-screenshot" type="file" -->

[sixel-chart.png]($output_dir/sixel-chart.png)

### Meshfox end card
<!-- meshfox:node id="meshfox-end-card" type="file" -->

[end-card.png]($output_dir/end-card.png)

