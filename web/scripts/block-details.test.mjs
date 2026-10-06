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
    {name:'lang', type:'select', choices:['en','hy'], required:false, default:'en'},
    {name:'pages', type:'int', choices:[], required:true, default:'09'},
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
