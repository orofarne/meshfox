import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';
import ReactMarkdown from 'react-markdown';
import { renderToStaticMarkup } from 'react-dom/server';
import remarkGfm from 'remark-gfm';
import remarkColorSpans from '../src/remarkColorSpans.ts';
import remarkSubSup from '../src/remarkSubSup.ts';

function render(source) {
  return renderToStaticMarkup(React.createElement(ReactMarkdown, {remarkPlugins: [remarkGfm, remarkColorSpans, remarkSubSup], children: source}));
}

test('colored spans retain nested Markdown, entities, escapes and outer colors', () => {
  const html = render('[outer **bold** [inner]{color="#abc"} a &amp; b \\*literal\\* tail]{color="#e06c75" background-color="#fff3b0"} plain');
  assert.match(html, /<span style="color:#e06c75;background-color:#fff3b0">outer <strong>bold<\/strong> <span style="color:#aabbcc">inner<\/span> a &amp; b \*literal\* tail<\/span> plain/);
  assert.match(render('H~2~O and [x^n^]{color=#abc}'), /H<sub>2<\/sub>O/);
  assert.match(render('[before [link](https://example.com) after]{color=#abc}'), /<span style="color:#aabbcc">before <a/);
});

test('code, escaped/entity delimiters, bad attributes and block boundaries stay literal', () => {
  for (const source of [
    '`[code]{color=#fff}`', '```\n[code]{color=#fff}\n```',
    '\\[escaped]{color=#fff}', '&#91;entity]{color=#fff}',
    '[bad]{color=red}', '[bad]{color=#fff color=#000}', '[bad]{color=&#35;fff}',
    '[cross\n\nparagraph]{color=#fff}', '[cross *emphasis]{color=#fff} end*',
    '![alt [text]{color=#fff}](https://example.com/image.png)',
  ]) assert.ok(!render(source).includes('<span'), source);
});
