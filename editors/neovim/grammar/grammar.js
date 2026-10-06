/**
 * Tree-sitter grammar for the small meshfox constructs that live inside
 * Markdown: `<!-- meshfox:... -->` marker comments, the attribute tail of a
 * fence info string (everything after the language) and a `form` fence's
 * `field ...` lines. Mirrors SPEC.md's "Formal grammar" section.
 *
 * It is never used standalone: Markdown's own tree injects it (see
 * queries/markdown/injections.scm), so the root has three alternative shapes,
 * told apart by their first token.
 *
 * Tag names and attribute keys are deliberately NOT enumerated: any
 * `meshfox:<name>` / any key parses, same as the TextMate grammar, so a new
 * marker or attribute added to the spec needs no change here.
 *
 * The regexes below avoid swallowing a glued-on `-->` (the spec allows
 * `<!-- meshfox:canvas-->`): a run of dashes is only part of a token when it
 * isn't the start of `-->`.
 */
module.exports = grammar({
  name: 'meshfox',

  extras: _ => [/\s/],

  rules: {
    source: $ => choice(
      repeat1($.field_line),
      repeat1($.attribute),
      seq($.marker, repeat(choice($.marker, $.text))),
    ),

    // <!-- meshfox:node id="x" -->   /   <!-- /meshfox:output -->
    marker: $ => seq('<!--', optional('/'), $.tag, repeat($.attribute), '-->'),

    tag: _ => /meshfox:[A-Za-z0-9_]+(-[A-Za-z0-9_]+)*/,

    // A form fence body line: `field var=x label="X"`.
    field_line: $ => seq('field', repeat($.attribute)),

    attribute: $ => seq(
      field('key', $.key),
      optional(seq('=', field('value', $.value))),
    ),

    // Any char but whitespace, `=` and `"` (spec: key-char).
    key: _ => /([^\s="\-]|-[^\s="\-]|--+[^\s=">\-])+/,

    value: $ => choice($.string, $.bare_value),

    string: _ => choice(/"[^"]*"/, /'[^']*'/),

    bare_value: _ => /([^\s"'\-]|-[^\s\-])([^\s\-]|-[^\s\-]|--+[^\s>\-])*/,

    // Prose after/between markers on one line:
    // `<!-- meshfox:comment -->Any text<!-- /meshfox:comment -->`.
    text: _ => choice(/[^<\s][^<]*/, '<'),
  },
});
