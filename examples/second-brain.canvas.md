<!-- meshfox:canvas -->
# Second Brain Demo
<!-- meshfox:node id="root" -->

A canvas as an LLM agent's persistent memory, outside the chat window itself
— each memory is one node, tagged by type (`user`/`feedback`/`project`/
`reference`). The constraint below enforces one rule an agent actually needs:
a `feedback`/`project` memory always carries a `**Why:**` line, so a future
session can judge an edge case instead of blindly following the rule with no
sense of when it doesn't apply. Run `meshfox check
examples/second-brain.canvas.md` to see it evaluate (all four nodes below are
meant to pass).

```starlark constraint
for n in self.descendants():
    if "feedback" in n.tags or "project" in n.tags:
        if "**Why:**" not in n.text:
            fail(n.id + " (" + ",".join(n.tags) + "): missing a '**Why:**' line")
```

## New to this repo's Rust half
<!-- meshfox:node id="new-to-this-repo-s-rust-half" tags="user" -->

Frontend engineer, ten years of React; this is their first time touching the
Rust backend half of this repo. No `**Why:**` required — a `user` memory is a
fact about the person, not a rule to apply selectively.

## Edit .canvas.md via the meshfox MCP tools
<!-- meshfox:node id="edit-canvas-md-via-the-meshfox-mcp-tools" tags="feedback" -->

Edit `*.canvas.md` files via the meshfox `node_*` MCP tools, not by hand-editing
the raw Markdown.

**Why:** a hand-edit risks corrupting node ids, `parent=`, or `meshfox:edge`
bookkeeping that the MCP tools keep consistent automatically.
**How to apply:** for any `*.canvas.md` file, reach for `canvas_open`/
`node_add`/`node_body`/etc. first; hand-edit only prose inside an existing
node's body.

## Reporting pipeline rewrite is retention-driven
<!-- meshfox:node id="reporting-pipeline-rewrite-is-retention-driven" tags="project" -->

The reporting pipeline rewrite is driven by a data-retention deadline, not a
performance goal.

**Why:** legal set a cutoff for how long raw event logs may be kept, and the
current pipeline reads directly from that raw table.
**How to apply:** any design choice in the rewrite should favor finishing
before the retention cutoff over squeezing out extra throughput.

## Incidents tracked in Linear, not GitHub Issues
<!-- meshfox:node id="incidents-tracked-in-linear-not-github-issues" tags="reference" -->

Incidents and outages are tracked in the "OPS" Linear project, not GitHub
Issues. No `**Why:**` required — a `reference` memory is a pointer to an
external system, not a rule.

