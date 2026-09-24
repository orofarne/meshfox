import { test, expect } from "@playwright/test";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import path from "node:path";

class McpClient {
  private nextId = 1;
  private buffer = "";
  private pending = new Map<number, { resolve: (value: any) => void; reject: (error: Error) => void }>();
  private constructor(private process: ChildProcessWithoutNullStreams) {
    process.stdout.setEncoding("utf8");
    process.stdout.on("data", (chunk: string) => {
      this.buffer += chunk;
      for (let newline; (newline = this.buffer.indexOf("\n")) >= 0;) {
        const line = this.buffer.slice(0, newline);
        this.buffer = this.buffer.slice(newline + 1);
        if (!line.trim()) continue;
        const message = JSON.parse(line);
        const pending = this.pending.get(message.id);
        if (!pending) continue;
        this.pending.delete(message.id);
        if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
        else pending.resolve(message.result);
      }
    });
    process.on("exit", (code) => {
      for (const pending of this.pending.values()) pending.reject(new Error(`MCP exited with ${code}`));
      this.pending.clear();
    });
  }

  static async start(fixtureDir: string) {
    const process = spawn("cargo", ["run", "-q", "--manifest-path", path.join(import.meta.dirname, "../../Cargo.toml"), "-p", "meshfox-cli", "--", "mcp"], {
      cwd: fixtureDir,
      stdio: "pipe",
    });
    const client = new McpClient(process);
    await client.request("initialize", {
      protocolVersion: "2025-03-26",
      capabilities: {},
      clientInfo: { name: "meshfox-web-e2e", version: "1.0.0" },
    });
    process.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
    return client;
  }

  request(method: string, params: object): Promise<any> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.process.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  async tool(name: string, args: object): Promise<any> {
    const result = await this.request("tools/call", { name, arguments: args });
    if (result.isError) throw new Error(`${name}: ${JSON.stringify(result.content)}`);
    return result.structuredContent;
  }

  close() { this.process.kill(); }
}

test("MCP edits appear in an already-open WebUI without reloading", async ({ page }, testInfo) => {
  const browser = testInfo.project.name.startsWith("firefox") ? "FIREFOX" : "CHROME";
  const fixtureDir = process.env[`MESHFOX_E2E_MCP_LIVE_${browser}_DIR`];
  if (!fixtureDir) throw new Error(`MESHFOX_E2E_MCP_LIVE_${browser}_DIR is unset`);
  const root = page.locator('.react-flow__node[data-id="mcp-live"]');
  const target = page.locator('.react-flow__node[data-id="target-node"]');
  await page.goto("/");
  await expect(root).toBeVisible();
  await expect(target).toBeVisible();

  const mcp = await McpClient.start(fixtureDir);
  try {
    const { canvas_id } = await mcp.tool("canvas_open", { path: "mcp-live.canvas.md" });
    const args = (extra: object) => ({ canvas_id, ...extra });

    await mcp.tool("node_meta", args({ node_id: "mcp-live", tags: "live-tag" }));
    await expect.poll(async () => {
      const canvas = await (await page.request.get("/api/canvas")).json();
      return canvas.nodes.find((node: { id: string }) => node.id === "mcp-live")?.tags;
    }).toEqual(["live-tag"]);
    await expect(root.locator(".mesh-tag-chip")).toContainText("live-tag");

    await mcp.tool("node_body", args({ node_id: "mcp-live", body: "After MCP body edit." }));
    await expect(root.locator(".mesh-node-body")).toContainText("After MCP body edit.");

    await mcp.tool("node_rename", args({ node_id: "mcp-live", title: "After MCP title edit" }));
    await expect(root.locator(".mesh-node-title-text, .mesh-node-title-centered-text")).toHaveText("After MCP title edit");

    const { node_id } = await mcp.tool("node_add", args({ parent_id: "mcp-live", title: "Added by MCP", body: "New node body.", x: 700, y: 300 }));
    const added = page.locator(`.react-flow__node[data-id="${node_id}"]`);
    await expect(added).toBeVisible();
    await expect(added).toContainText("Added by MCP");

    await mcp.tool("node_edges", args({ node_id, from: ["target-node"] }));
    const extraEdge = page.locator(`.react-flow__edge[data-id="target-node->${node_id}:extra"]`);
    await expect(extraEdge).toHaveCount(1);
    await expect(extraEdge.locator("path.react-flow__edge-path")).toHaveAttribute("d", /\S/);

    await mcp.tool("node_edges", args({ node_id, from: [] }));
    await expect(extraEdge).toHaveCount(0);

    await mcp.tool("node_rm", args({ node_id }));
    await expect(added).toHaveCount(0);
  } finally {
    mcp.close();
  }
});
