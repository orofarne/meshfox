// The headers the editor must be served with. One list, used by the preview
// server the tests run against and written into the build as `_headers`
// (Netlify / Cloudflare Pages format; the README shows the nginx equivalent),
// so what is tested is what is deployed.
//
// The Content-Security-Policy is the second line of defence: a blob URL
// inherits the policy of the page that made it, so even a document that got a
// script into a blob page could not run it or send anything out.
export const CSP = [
  "default-src 'self'",
  // The page's own modules; WebAssembly needs 'wasm-unsafe-eval'. No inline scripts.
  "script-src 'self' 'wasm-unsafe-eval'",
  // React and Monaco set `style` attributes.
  "style-src 'self' 'unsafe-inline'",
  // Pictures from the opened files (blob:), data: images, and — by decision —
  // remote images a document links to.
  "img-src 'self' blob: data: https: http:",
  "font-src 'self' data:",
  // Nothing may be sent anywhere else, whatever runs in the page.
  "connect-src 'self' blob:",
  "worker-src 'self' blob:",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
  // Embedding is not offered (yet).
  "frame-ancestors 'none'",
].join("; ");

export const SECURITY_HEADERS: Record<string, string> = {
  "Content-Security-Policy": CSP,
  "X-Content-Type-Options": "nosniff",
  // Following a link out of a document must not tell the other site what was open.
  "Referrer-Policy": "no-referrer",
  "Cross-Origin-Opener-Policy": "same-origin",
  "X-Frame-Options": "DENY",
};

/** Content-hashed files never change; the page that names them always may. */
export const CACHE_RULES: Array<{ path: string; headers: Record<string, string> }> = [
  { path: "/assets/*", headers: { "Cache-Control": "public, max-age=31536000, immutable" } },
  { path: "/index.html", headers: { "Cache-Control": "no-cache" } },
  { path: "/", headers: { "Cache-Control": "no-cache" } },
];

/** The `_headers` file for the build. */
export function headersFile(): string {
  const block = (path: string, headers: Record<string, string>) =>
    `${path}\n${Object.entries(headers).map(([k, v]) => `  ${k}: ${v}`).join("\n")}\n`;
  return [
    block("/*", SECURITY_HEADERS),
    ...CACHE_RULES.map((r) => block(r.path, r.headers)),
    block("/*.wasm", { "Content-Type": "application/wasm" }),
  ].join("\n");
}
