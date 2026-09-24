import { test, expect } from "@playwright/test";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { launchVSCode, openMeshfoxCanvas } from "./helpers";

const MESHFOX_BIN = join(import.meta.dirname, "../../../target/debug/meshfox");

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

  static async start(workspaceDir: string) {
    const process = spawn(MESHFOX_BIN, ["mcp"], { cwd: workspaceDir, stdio: "pipe" });
    const client = new McpClient(process);
    await client.request("initialize", {
      protocolVersion: "2025-03-26",
      capabilities: {},
      clientInfo: { name: "meshfox-vscode-e2e", version: "1.0.0" },
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

test("MCP edits appear in an already-open VS Code canvas without reloading", async () => {
  // This spec tests the extension's own worker, independent of a user's
  // configured daemon. The MCP process inherits the same override.
  const previousSocket = process.env.MESHFOX_SERVER_SOCKET;
  process.env.MESHFOX_SERVER_SOCKET = "";
  let session: Awaited<ReturnType<typeof launchVSCode>> | undefined;
  let mcp: McpClient | undefined;
  try {
    session = await launchVSCode("mcp-live.canvas.md", { MESHFOX_SERVER_SOCKET: "" });
    mkdirSync(join(session.workspaceDir, ".meshfox"), { recursive: true });
    writeFileSync(join(session.workspaceDir, ".meshfox", "config.toml"), 'server_socket = ""\n');
    const frame = await openMeshfoxCanvas(session.page);
    const root = frame.locator('.react-flow__node[data-id="mcp-live"]');
    const target = frame.locator('.react-flow__node[data-id="target-node"]');
    await expect(root).toBeVisible();
    await expect(target).toBeVisible();

    mcp = await McpClient.start(session.workspaceDir);
    const { canvas_id } = await mcp.tool("canvas_open", { path: "mcp-live.canvas.md" });
    const args = (extra: object) => ({ canvas_id, ...extra });

    await mcp.tool("node_meta", args({ node_id: "mcp-live", tags: "live-tag" }));
    await expect(root.locator(".mesh-tag-chip")).toContainText("live-tag");

    await mcp.tool("node_body", args({ node_id: "mcp-live", body: "After MCP body edit." }));
    await expect(root.locator(".mesh-node-body")).toContainText("After MCP body edit.");

    await mcp.tool("node_rename", args({ node_id: "mcp-live", title: "After MCP title edit" }));
    await expect(root.locator(".mesh-node-title-text, .mesh-node-title-centered-text")).toHaveText("After MCP title edit");

    const { node_id } = await mcp.tool("node_add", args({ parent_id: "mcp-live", title: "Added by MCP", body: "New node body.", x: 700, y: 300 }));
    const added = frame.locator(`.react-flow__node[data-id="${node_id}"]`);
    await expect(added).toBeVisible();
    await expect(added).toContainText("Added by MCP");

    await mcp.tool("node_edges", args({ node_id, from: ["target-node"] }));
    const edge = frame.locator(`.react-flow__edge[data-id="target-node->${node_id}:extra"]`);
    await expect(edge).toHaveCount(1);
    await expect(edge.locator("path.react-flow__edge-path")).toHaveAttribute("d", /\S/);

    await mcp.tool("node_edges", args({ node_id, from: [] }));
    await expect(edge).toHaveCount(0);

    await mcp.tool("node_rm", args({ node_id }));
    await expect(added).toHaveCount(0);
  } finally {
    mcp?.close();
    if (session) {
      await session.app.close();
      session.cleanup();
    }
    if (previousSocket === undefined) delete process.env.MESHFOX_SERVER_SOCKET;
    else process.env.MESHFOX_SERVER_SOCKET = previousSocket;
  }
});
