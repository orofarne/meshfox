;; extends

; `<!-- meshfox:... -->` marker comments (block level: a comment starting a line).
; NB: `#match?` is a Vim regex run in very-magic mode, so a literal `<` must
; be written `\\<` (bare `<` means "start of word").
((html_block) @injection.content
  (#match? @injection.content "^\\s*\\<!--\\s*/?meshfox:")
  (#set! injection.language "meshfox"))

; Attributes in a fence's info string (` ```bash name="x" cache `). The
; `(language)` child is excluded from the injected range by default, so the
; meshfox parser only ever sees what follows the language.
(fenced_code_block
  (info_string) @injection.content
  (#match? @injection.content "^\\S+\\s+\\S")
  (#set! injection.language "meshfox"))

; ` ```form ` bodies are `field var=... label=...` lines.
(fenced_code_block
  (info_string
    (language) @_lang)
  (code_fence_content) @injection.content
  (#eq? @_lang "form")
  (#set! injection.language "meshfox"))
