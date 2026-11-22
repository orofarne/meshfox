// LaTeX math in rendered Markdown: `$…$` / `$$…$$` parsed by `remark-math`,
// typeset by KaTeX (client-side, no network). `strict: "ignore"` keeps KaTeX's
// console warnings out of the browser for TeX it merely finds unusual, and
// `throwOnError: false` shows a broken formula as red source text instead of
// dropping the whole node. `trust` stays off (KaTeX's `\href`/`\includegraphics`
// style commands stay inert).
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";

export const mathRemarkPlugin = remarkMath;
export const mathRehypePlugins = [[rehypeKatex, { throwOnError: false, strict: "ignore" }]] as never;
