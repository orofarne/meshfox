<!-- meshfox:canvas -->
# Extending Blocks
<!-- meshfox:node id="root" -->

`extends="parent"` puts the code of another block **in front of** this one's,
and runs the whole text as one script. That gives the *definitions / action /
tests* split without a module system, in any language: the definitions block
holds only functions, and each block that extends it adds the one thing it
does with them — run the pipeline step, or test the functions.

The extended block is not run and not a dependency: nothing about it is
executed for the extending block. Only its text is borrowed, so the extending
block must still declare its own `deps=`, `inputs=` and `outputs=`.

## Python
<!-- meshfox:node id="py" -->

The definitions carry the `interpreter=`; blocks that extend them inherit it
(and any `env=` the definitions list), so it is written once.

```python name="slug-defs" interpreter="python3 -u"
import re

def slug(text):
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
```

The action adds one line to the definitions:

```python name="slug" default extends="slug-defs"
print(slug("Hello, Extending World!"))
```

The tests add theirs. They see every name the definitions define, private or
not, because the two are one script:

```python name="slug-test" extends="slug-defs"
import unittest

class SlugTests(unittest.TestCase):
    def test_lowercases_and_joins_words(self):
        self.assertEqual(slug("Hello World"), "hello-world")

    def test_drops_punctuation_at_the_edges(self):
        self.assertEqual(slug("  --Hi!--  "), "hi")

unittest.main(argv=["slug-test"], verbosity=2)
```

## Shell
<!-- meshfox:node id="sh" -->

The same split works where there is no `import` at all. Extended blocks may be
chained and are included once, in order, even when several paths reach them:

```bash name="log-defs"
say() { printf '[%s] %s\n' "$1" "$2"; }
```

```bash name="greet-defs" extends="log-defs"
greet() { say greet "hello, $1"; }
```

```bash name="greet" extends="greet-defs"
greet "$(whoami)"
```

```bash name="greet-test" extends="greet-defs,log-defs"
out="$(greet world)"
[ "$out" = "[greet] hello, world" ] || { echo "FAIL: $out"; exit 1; }
echo "ok"
```

## Running it

```text
meshfox examples/extends.canvas.md run py slug-test    # test the definitions
meshfox examples/extends.canvas.md run py slug         # run the action
meshfox examples/extends.canvas.md run sh greet-test
```

An error line number is a line of the **joined** text: the extended code comes
first, so a line of the extending block is reported shifted by the length of
what it extends. The definitions' own lines are exact.
