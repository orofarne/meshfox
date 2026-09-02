const shell = (body: string, csp: string) => `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<style>
  html, body { height: 100%; margin: 0; padding: 0; }
  body {
    font-family: var(--vscode-font-family, sans-serif);
    color: var(--vscode-foreground);
    background: var(--vscode-editor-background);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  iframe { position: fixed; inset: 0; width: 100%; height: 100%; border: 0; }
  pre { white-space: pre-wrap; max-width: 60ch; }
</style>
</head>
<body>${body}</body>
</html>`;

export function loadingHtml(): string {
  return shell("<p>Starting meshfox…</p>", "default-src 'none'; style-src 'unsafe-inline';");
}

export function errorHtml(message: string): string {
  return shell(`<pre>meshfox failed to start:\n\n${escapeHtml(message)}</pre>`, "default-src 'none'; style-src 'unsafe-inline';");
}

/** `src`'s own origin is folded into the CSP dynamically because
 * `vscode.env.asExternalUri` doesn't return a fixed host — a plain local
 * desktop window returns the `http://127.0.0.1:<port>` URI unchanged, but
 * a remote/Codespaces window proxies it through a per-session host this
 * extension can't hardcode in advance. */
export function canvasHtml(src: string): string {
  const origin = new URL(src).origin;
  return shell(`<iframe src="${escapeHtml(src)}"></iframe>`, `default-src 'none'; frame-src ${origin}; style-src 'unsafe-inline';`);
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}
