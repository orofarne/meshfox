/** The files the host hands the wasm renderer: an in-memory tree, keyed by
 * `/`-separated paths relative to its root (a picked folder, or the repo for
 * the bundled examples). Binary assets that are only ever *displayed* (images)
 * can instead be registered by URL and fetched on demand. */
export class VirtualFiles {
  private readonly bytes = new Map<string, Uint8Array>();
  private readonly urls = new Map<string, string>();

  /** Folds `.`/`..`, drops empty segments and any leading `/`. */
  static norm(path: string): string {
    const out: string[] = [];
    for (const part of path.replaceAll("\\", "/").split("/")) {
      if (part === "" || part === ".") continue;
      if (part === "..") out.pop();
      else out.push(part);
    }
    return out.join("/");
  }

  static dirname(path: string): string {
    const i = path.lastIndexOf("/");
    return i < 0 ? "" : path.slice(0, i);
  }

  static join(dir: string, rel: string): string {
    return VirtualFiles.norm(dir ? `${dir}/${rel}` : rel);
  }

  set(path: string, data: Uint8Array | string): void {
    this.bytes.set(VirtualFiles.norm(path), typeof data === "string" ? new TextEncoder().encode(data) : data);
  }

  setUrl(path: string, url: string): void {
    this.urls.set(VirtualFiles.norm(path), url);
  }

  get(path: string): Uint8Array | undefined {
    return this.bytes.get(VirtualFiles.norm(path));
  }

  urlOf(path: string): string | undefined {
    return this.urls.get(VirtualFiles.norm(path));
  }

  text(path: string): string | undefined {
    const data = this.get(path);
    return data === undefined ? undefined : new TextDecoder().decode(data);
  }

  paths(): string[] {
    return [...this.bytes.keys()].sort((a, b) => a.localeCompare(b));
  }

  get size(): number {
    return this.bytes.size;
  }
}
