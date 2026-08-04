import {
  Decoration,
  type DecorationSet,
  EditorView,
  MatchDecorator,
  ViewPlugin,
  type ViewUpdate,
} from "@codemirror/view";
import type { Extension } from "@codemirror/state";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { languages } from "@codemirror/language-data";

const commentMark = Decoration.mark({ class: "cm-meshfoxComment" });
const keywordMark = Decoration.mark({ class: "cm-meshfoxKeyword" });
const attrNameMark = Decoration.mark({ class: "cm-meshfoxAttrName" });
const attrValueMark = Decoration.mark({ class: "cm-meshfoxAttrValue" });

/**
 * `key`, `key=value`, `key="quoted value"` — the one attribute shape
 * shared by every meshfox HTML comment (`id="x" x=0 cache`) and every
 * runnable fence's info string (```bash name="build" cache
 * deps="build"```, see SPEC.md). Marks the key and, if present, the value
 * (including its quotes) as two separate decorations. `base` is `text`'s
 * own absolute position in the document, since `text` is always a slice
 * already pulled out of a MatchDecorator match.
 */
function addAttrMarks(add: (from: number, to: number, deco: Decoration) => void, text: string, base: number) {
  const re = /([\w-]+)(=(?:"[^"]*"|'[^']*'|[^\s`>]*))?/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    const nameStart = base + m.index;
    const nameEnd = nameStart + m[1].length;
    add(nameStart, nameEnd, attrNameMark);
    if (m[2] && m[2].length > 1) add(nameEnd + 1, nameStart + m[0].length, attrValueMark);
  }
}

// `<!-- meshfox:node id="x" x=0 y=0 -->`, `<!-- meshfox:edge from="y" -->`,
// `<!-- /meshfox:output -->`, `<!-- meshfox:var name="X" ... -->` — every
// bookkeeping comment SPEC.md's "File structure"/"Variables"/"Cached
// output" define, always written on a single line.
const commentMatcher = new MatchDecorator({
  regexp: /<!--\s*\/?meshfox:[\w-]+(?:\s+[\w-]+(?:=(?:"[^"]*"|'[^']*'|[^\s>]*))?)*\s*-->/g,
  decorate(add, from, to, match) {
    add(from, to, commentMark);
    const body = match[0];
    const kwStart = body.indexOf("meshfox:");
    const kwLead = body[kwStart - 1] === "/" ? kwStart - 1 : kwStart;
    const kwTail = /^[\w-]*/.exec(body.slice(kwStart + "meshfox:".length))![0];
    const kwEnd = kwStart + "meshfox:".length + kwTail.length;
    add(from + kwLead, from + kwEnd, keywordMark);
    // Everything between the keyword and the closing "-->" is attributes.
    addAttrMarks(add, body.slice(kwEnd, body.length - "-->".length), from + kwEnd);
  },
});

// ```bash name="build" cache deps="build" env="X"``` — a runnable fence's
// info-string attributes (SPEC.md's "Runnable code fences"); `sh` is the
// same language's alias. Only the attributes are decorated here — the
// language word and fence markers already get the markdown language's own
// styling.
const fenceMatcher = new MatchDecorator({
  regexp: /```(?:bash|sh)\b((?:\s+[\w-]+(?:=(?:"[^"]*"|'[^']*'|[^\s`]*))?)*)/g,
  decorate(add, from, to, match) {
    const attrs = match[1];
    if (!attrs) return;
    addAttrMarks(add, attrs, from + match[0].length - attrs.length);
  },
});

function plugin(matcher: MatchDecorator) {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = matcher.createDeco(view);
      }
      update(update: ViewUpdate) {
        this.decorations = matcher.updateDeco(update, this.decorations);
      }
    },
    { decorations: (v) => v.decorations },
  );
}

/**
 * Highlights meshfox's own syntax extensions on top of `@codemirror/lang-
 * markdown`'s ordinary Markdown highlighting — the `meshfox:...` HTML
 * comments (`meshfox:node`, `meshfox:edge`, `meshfox:output`, `meshfox:
 * var`, `meshfox:canvas`) and runnable fences' `name=`/`cache`/`deps=`/
 * `env=`/`default` attributes — so the document's actual structure reads
 * at a glance instead of blending into a wall of dimmed HTML-comment text.
 * Plain decoration overlays (regex-driven, via `MatchDecorator`) rather
 * than a real language grammar change, since meshfox's extensions are a
 * strict subset of what's already valid Markdown/HTML — nothing here
 * needs its own parser.
 */
export const meshfoxSyntaxHighlighting: Extension = [plugin(commentMatcher), plugin(fenceMatcher)];

/** Both of the editor's CodeMirror extensions — plain Markdown plus
 * meshfox's own syntax highlighting on top — bundled together since every
 * editor in the app (NodeTextEditor, CanvasSourceEditor) wants both. */
export const meshfoxMarkdown: Extension = [
  markdown({ base: markdownLanguage, codeLanguages: languages }),
  meshfoxSyntaxHighlighting,
];
