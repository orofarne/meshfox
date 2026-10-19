; Every pattern carries `priority 125` (the default is 100). Markdown also
; injects its own `html` language into the very same `<!-- ... -->` ranges,
; and that layer's `@comment` is painted after ours; without a higher
; priority the whole marker comes out as a plain comment.

([
  "<!--"
  "-->"
] @punctuation.bracket
  (#set! priority 125))

("/" @punctuation.delimiter
  (#set! priority 125))

((tag) @keyword
  (#set! priority 125))

("field" @keyword
  (#set! priority 125))

((key) @property
  (#set! priority 125))

("=" @operator
  (#set! priority 125))

((string) @string
  (#set! priority 125))

((bare_value) @string
  (#set! priority 125))
