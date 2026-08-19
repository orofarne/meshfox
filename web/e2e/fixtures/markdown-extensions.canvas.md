<!-- meshfox:canvas -->
# Markdown Extensions Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright suite covering three of meshfox's own
narrow Markdown extensions (`web/e2e/markdown-extensions.spec.ts` —
TODO.canvas.md's "Формальные граматики для meshfox:*" subtree): image
size attributes, subscript/superscript, and GFM alert blockquotes. One
bare root is enough — this is read-only rendering, nothing here is ever
edited by the suite.

![alt](https://example.com/pic.png){width=300 height=50%}

H~2~O and x^n^

> [!WARNING]
> be careful here

> just a quote

An indented (4-space) fence example, same escaping trick SPEC.md's own
"Runnable code fences" section uses to show the syntax as inert
documentation rather than a real runnable block:

    ```bash name="not-really-runnable"
    echo hi
    ```
