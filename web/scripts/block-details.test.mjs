import assert from 'node:assert/strict';
import test from 'node:test';
import { parseBody, parseEnvList, selectedEnvNames } from '../src/fence.ts';

test('visible block contract retains declaration order, UI-only defaults and file templates', () => {
  const source = `<!-- meshfox:arg name="lang" type="select" choices="en,hy" default="en" -->

<!-- meshfox:arg name='pages' type='int' default='09' required -->
\`\`\`bash name="extract" inputs="pdf_\${lang}.pdf, index.txt" outputs="csv_\${lang}.csv"
echo ok
\`\`\``;
  const block = parseBody(source, 'root').find(s => s.type === 'code');
  assert.deepEqual(block.args, [
    {name:'lang', type:'select', choices:['en','hy'], choicesVar:undefined, required:false, default:'en'},
    {name:'pages', type:'int', choices:[], choicesVar:undefined, required:true, default:'09'},
  ]);
  assert.deepEqual(block.inputs, ['pdf_\${lang}.pdf', 'index.txt']);
  assert.deepEqual(block.outputs, ['csv_\${lang}.csv']);
});

test('signatures do not attach across prose or leak to another block', () => {
  const source = `<!-- meshfox:arg name="unused" -->
Prose interrupts attachment.
\`\`\`bash name="first"
echo first
\`\`\`
<!-- meshfox:arg name="lang" -->
\`\`\`button name="launch"
Run
\`\`\`
\`\`\`bash name="last"
echo last
\`\`\``;
  const blocks = parseBody(source,'root').filter(s => s.type === 'code');
  assert.deepEqual(blocks.map(b=>b.args.map(a=>a.name)), [[],['lang'],[]]);
  assert.equal(blocks[1].args[0].required, true);
});


test('env name templates retain source spelling and show concrete application selections', () => {
  assert.deepEqual(parseEnvList('PDF_URL=PDF_URL_${lang},VALUE=${arg},X=$GLOBAL'), [
    {localName:'PDF_URL', varName:'PDF_URL_${lang}'},
    {localName:'VALUE', varName:'${arg}'},
    {localName:'X', varName:'GLOBAL'},
  ]);
  assert.equal(selectedEnvNames('PDF_URL_${lang}', 'fetch[lang=hy]'), 'PDF_URL_hy');
  assert.equal(selectedEnvNames('PDF_URL_${lang}', 'fetch[lang="hy",query="ACME, Inc."]'), 'PDF_URL_hy');
  assert.equal(selectedEnvNames('PDF_URL_${lang}', 'fetch'), null);
  assert.equal(selectedEnvNames('PDF_URL_${lang}', 'fetch[lang="${other}"]'), null);
});

test('exports match exact producers across nodes and retain declared types', async () => {
  const {parseVarDecls, exportsForBlock} = await import('../src/vars.ts');
  const decls = parseVarDecls({nodes: [
    {id:'root', text: `<!-- meshfox:var name="PDF_URL_en" from="download/observe" -->
<!-- meshfox:var name="COUNT" type="int" from="download/observe" -->
<!-- meshfox:var name="OTHER" from="other/observe" -->
<!-- meshfox:var name="CONFIG" default="configured" -->`},
    {id:'download', text:'<!-- meshfox:var name="LOCAL" type="bool" from="observe" -->'},
  ]});
  assert.deepEqual(exportsForBlock(decls, 'download', 'observe').map(v=>[v.name,v.type]), [
    ['PDF_URL_en','string'], ['COUNT','int'], ['LOCAL','bool'],
  ]);
  assert.deepEqual(exportsForBlock(decls,'other','observe').map(v=>v.name), ['OTHER']);
  assert.deepEqual(exportsForBlock(decls,'download','fetch'), []);
});

test('example code and cached output never become declared exports', async () => {
  const {parseVarDecls, exportsForBlock} = await import('../src/vars.ts');
  const decls = parseVarDecls({nodes:[{id:'root', text: `
<!-- meshfox:var name="REAL" from="observe" -->
\`\`\`\`markdown
<!-- meshfox:var name="EXAMPLE" from="observe" -->
\`\`\`
<!-- meshfox:var name="NESTED" from="observe" -->
\`\`\`\`
~~~text
<!-- meshfox:var name="TILDE" from="observe" -->
~~~
    <!-- meshfox:var name="INDENTED" from="observe" -->
<!-- meshfox:output name="show" -->
<!-- meshfox:var name="OUTPUT" from="observe" -->
<!-- /meshfox:output -->
<!-- meshfox:variable name="WRONG_MARKER" from="observe" -->`} ]});
  assert.deepEqual(exportsForBlock(decls,'root','observe').map(v=>v.name), ['REAL']);
});


test('@service YAML is rendered as a managed service without a redundant flag', () => {
  const block = parseBody('```yaml name="app" interpreter="@service"\nstart: sleep 30\n```', 'root').find(s => s.type === 'code');
  assert.equal(block.service, true);
  assert.equal(block.interpreter, '@service');
});

test('tangle flag follows the core: bare or true enables it, false disables it', () => {
  const blocks = parseBody(
    ['tangle', 'tangle=true', 'tangle=false', ''].map((flag, i) =>
      `\`\`\`ruby name="b${i}" interpreter="ruby" ${flag}\nputs 1\n\`\`\``).join('\n'),
    'root').filter(s => s.type === 'code');
  assert.deepEqual(blocks.map(b => b.tangle), [true, true, false, false]);
});

test('native meshfox calls have ordinary runnable controls and implicit defaults', () => {
  const source = '```meshfox\ncanvas: ../README.md\nrun: install-from-source\n```\n';
  const blocks = parseBody(source, 'build').filter(s => s.type === 'code');
  assert.equal(blocks.length, 1);
  assert.equal(blocks[0].lang, 'meshfox');
  assert.equal(blocks[0].name, 'build');
  assert.match(blocks[0].code, /canvas: ../);
});

test('lease stays distinct from deps while participating in dependency planning', () => {
  for (const lang of ['sh', 'button']) {
    for (const deps of ['', 'deps="prepare"']) {
      const block = parseBody([
        '```' + lang + ' name="tests" ' + deps + ' lease="lima, db/service" fold',
        'echo tests',
        '```',
      ].join('\n'), 'root').find(s => s.type === 'code');
      assert.deepEqual(block.declaredDeps, deps ? ['prepare'] : []);
      assert.deepEqual(block.leases, ['lima', 'db/service']);
      assert.deepEqual(block.deps, [...(deps ? ['prepare'] : []), 'lima', 'db/service']);
    }
  }
});

test('Starlark computed views are separate from executable blocks and constraints', () => {
  const source = '```starlark render name="counts" output="markdown"\nprint(1)\n```\n```starlark constraint\npass\n```\n```starlark\nprint(2)\n```';
  const segments = parseBody(source, 'root');
  assert.equal(segments.filter(s => s.type === 'render').length, 1);
  assert.equal(segments.filter(s => s.type === 'constraint').length, 1);
  assert.equal(segments.filter(s => s.type === 'code').length, 0);
  assert.equal(segments.find(s => s.type === 'render').name, 'counts');
  assert.ok(segments.find(s => s.type === 'markdown').content.includes('print(2)'));
});

test('Starlark views respect fence boundaries and leave nested examples inert', () => {
  const source = '~~~~markdown\n```starlark render\nprint("inert")\n```\n~~~~\n~~~~starlark render name="real"\nprint(1)\n~~~~~\n````starlark render\nprint(2)\n````';
  const segments = parseBody(source, 'root');
  assert.deepEqual(segments.filter(s => s.type === 'render').map(s => s.code), ['print(1)', 'print(2)']);
});
