;; extends

; Marker comments that don't start a block, e.g. the close marker in
; `Any text<!-- /meshfox:comment -->`.
((html_tag) @injection.content
  (#match? @injection.content "^\\s*\\<!--\\s*/?meshfox:")
  (#set! injection.language "meshfox"))
