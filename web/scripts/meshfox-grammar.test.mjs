import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createHighlighter } from "shiki";

const grammars = [
  ["web/Monaco", "../src/grammars/meshfox.tmLanguage.json", "markdown"],
  ["TUI source grammar", "../../crates/cli/src/grammars/meshfox.tmLanguage.json", "meshfox-markdown"],
  ["VS Code", "../../editors/vscode/syntaxes/meshfox-marker.injection.json", "markdown"],
];

for (const [label, path, language] of grammars) {
  test(`${label}: typed args and quoted application addresses retain their scopes`, async () => {
    const grammar = JSON.parse(await readFile(new URL(path, import.meta.url), "utf8"));
    // VS Code registers injectTo in package.json; Shiki needs it on the grammar.
    if (label === "VS Code") {
      grammar.name = "meshfox-vscode-marker";
      grammar.injectTo = ["text.html.markdown"];
    }
    const highlighter = await createHighlighter({
      langs: ["markdown", grammar], themes: ["github-light"],
    });
    try {
      const spans = (source) => highlighter.codeToTokens(source, {
        lang: language, theme: "github-light", includeExplanation: true,
      }).tokens.flat().flatMap((token) => token.explanation ?? []);
      const hasScope = (span, prefix) => span.scopes.some((scope) => scope.scopeName.startsWith(prefix));
      const args = spans(`<!-- meshfox:arg name="lang" type="select" choices="en,hy" prompt='Choose language' required -->`);
      assert.ok(args.some((span) => span.content === "meshfox:arg" && hasScope(span, "keyword.")));
      for (const name of ["name", "type", "choices", "prompt", "required"]) {
        assert.ok(args.some((span) => span.content === name && hasScope(span, "entity.other.attribute-name.meshfox")), name);
      }
      assert.ok(args.some((span) => span.content.includes("'Choose language'") && hasScope(span, "string.")));
      const application = `extract[query="ACME, Inc.",lang=hy]`;
      const output = spans(`<!-- meshfox:output block='${application}' -->`);
      assert.ok(output.some((span) => span.content.includes(`'${application}'`) && hasScope(span, "string.")),
        "inner JSON quotes and comma must stay inside one outer single-quoted value");
      const envTemplate = 'env="PDF_URL=PDF_URL_${lang}"';
      const fence = highlighter.codeToTokens('```bash name="fetch" confirm ' + envTemplate + '\necho "$PDF_URL"\n```', {
        lang: language, theme: "github-light", includeExplanation: true,
      });
      assert.ok(fence.tokens[0].map(token => token.content).join("").includes(envTemplate),
        "argument placeholders must stay intact in the fence info string");
      assert.ok(fence.tokens[0].map(token => token.content).join("").includes(" confirm "),
        "confirmation flags remain intact in the fence info string");
      const templateAttribute = spans('<!-- meshfox:node env="PDF_URL=PDF_URL_${lang}" -->');
      assert.ok(templateAttribute.some(span => span.content.includes('PDF_URL_${lang}') && hasScope(span, "string.")),
        "braces/dollars must stay inside an attribute string");
    } finally {
      highlighter.dispose();
    }
  });
}
