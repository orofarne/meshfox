import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { MESHFOX_VIEW } from "./e2e/meshfox-bin";

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-starlark-render-e2e-"));
fs.writeFileSync(path.join(dir, "render.canvas.md"), `# Render demo
<!-- meshfox:node id="root" x=0 y=0 width=520 -->
\`\`\`starlark render name="tag-counts"
counts = {}
for n in [doc] + doc.descendants():
    for tag in n.tags:
        counts[tag] = counts.get(tag, 0) + 1
print(md.table(headers=["Tag", "Nodes"], rows=[[md.link(tag, "https://example.com/" + tag), counts[tag]] for tag in sorted(counts)]))
\`\`\`
## Task
<!-- meshfox:node id="task" tags="todo,task" x=600 y=0 -->
A task.
## Broken view
<!-- meshfox:node id="broken" x=0 y=420 width=520 -->
\`\`\`starlark render name="broken"
undefined_name()
\`\`\`
`);
process.env.MESHFOX_SERVER_SOCKET = "";
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const port = 4647;
export default defineConfig({
  testDir: "./e2e",
  testMatch: "starlark-render.spec.ts",
  workers: 1,
  use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${port}`, viewport: { width: 1440, height: 1000 } },
  webServer: {
    command: `${MESHFOX_VIEW} ${dir}/render.canvas.md --port ${port} --no-open --no-auto-exit`,
    url: `http://127.0.0.1:${port}/api/canvas`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
