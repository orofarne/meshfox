<!-- meshfox:canvas -->
# A single-node document
<!-- meshfox:node id="root" -->

A canvas with just its root becomes a full-page document in the web UI's view mode. The toolbar stays available, and runnable blocks work just as they do on the canvas.

## Try it live

1. Read and scroll this page: there is no canvas grid, pan or zoom.
2. Run the block below to see its output on the page.
3. Click **Edit** to return to the regular canvas and add a child node.
4. Leave edit mode: with a child, the document stays a canvas. Remove the child to return to this page layout.

The layout is automatic. No document option is needed, and the root's saved position and size stay unchanged.

## A runnable block

```bash name="hello"
echo 'Hello from a single-node document!'
printf '\nThe same block works in page and canvas views.\n'
```

## A small comparison

| Document | View mode | Edit mode |
| --- | --- | --- |
| Root only | Full-page document | Canvas |
| Root with descendants | Canvas | Canvas |

## Ordinary Markdown

Headings, **bold text**, links, tables and code use the same renderer as a node's expanded view. Search is available from the toolbar or with `/`.

> This is still one node. Markdown headings inside its body are sections of the page, not child nodes.

## Some room to read

A single node can hold a guide, a notebook entry or a small runnable recipe. Its text flows down the page instead of being constrained by a saved card height.

Wide code blocks and tables keep their own horizontal scrolling. On a narrow screen the page adapts to the available width.

## Back to a canvas

When the document grows into a tree, its usual spatial view returns automatically. Folding that tree back down to one visible card does not activate this layout: the document must actually contain only its root.

