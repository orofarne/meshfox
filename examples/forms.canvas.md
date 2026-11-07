<!-- meshfox:canvas -->
# Forms: send, reset, keep
<!-- meshfox:node id="root" -->

A tour of `form` fences (see SPEC.md's "Form fences"): a small purchase log.
A form is a set of input fields for variables; **nothing happens while you type**.
Pressing **Send** commits all the fields at once and starts every `autorun`
block that uses them.

| Piece | What it does |
| --- | --- |
| `field var="X" label="..."` | one input per declared variable; without `label` the variable's `prompt` is the caption |
| `send="Add"` | the caption of the Send button |
| `reset` | after a Send whose `autorun` blocks all succeeded, the fields go back to their defaults — handy for entering a series of records |
| `keep` (on a `field`) | that field survives the automatic reset (the shop stays selected for the next purchase) |
| **Reset** button | puts every field back to its default by hand, `keep` ones included, and runs nothing |

The purchases are written to a temporary file (`$TMPDIR/meshfox-forms-example.tsv`),
so running the example leaves nothing in the repository.

<!-- meshfox:var name="ITEM" prompt="Item" default="" -->
<!-- meshfox:var name="AMOUNT" type="int" prompt="Amount" default="1" -->
<!-- meshfox:var name="SHOP" type="select" choices="corner shop,market,bakery" prompt="Shop" default="corner shop" -->

## Add a purchase
<!-- meshfox:node id="add-a-purchase" -->

Fill in the item and press **Add**. The block below runs, appends the purchase
and prints the whole log; the item and the amount are then cleared, while the
shop stays (`keep`). Try pressing **Add** with an empty item: the block fails, and
because it failed the form is **not** reset, so you can fix the input and send
again.

```form name="purchase" send="Add" reset
field var="ITEM" label="What did you buy?"
field var="AMOUNT"
field var="SHOP" keep
```

```bash name="add" autorun output="markdown" env="ITEM,AMOUNT,SHOP"
set -eu
if [ -z "$ITEM" ]; then
  echo "Item is empty - nothing added." >&2
  exit 1
fi
file="${TMPDIR:-/tmp}/meshfox-forms-example.tsv"
[ -f "$file" ] || printf 'item\tamount\tshop\n' > "$file"
printf '%s\t%s\t%s\n' "$ITEM" "$AMOUNT" "$SHOP" >> "$file"
echo "| Item | Amount | Shop |"
echo "|---|---:|---|"
tail -n +2 "$file" | while IFS="$(printf '\t')" read -r i a s; do
  echo "| $i | $a | $s |"
done
```

```bash name="clear-log" confirm
rm -f "${TMPDIR:-/tmp}/meshfox-forms-example.tsv"
echo "log cleared"
```

## From the command line and from an agent
<!-- meshfox:node id="from-the-command-line-and-from-an-agent" -->

A form is sent, not run, so `meshfox run` refuses it. The equivalents of its
buttons take the id of the node that holds the form (`add-a-purchase`) and the
form's name (`purchase`):

```text
# Send: commit the values, start the autorun blocks and wait for them
meshfox form submit --canvas examples/forms.canvas.md add-a-purchase purchase \
  --set ITEM=bread --set AMOUNT=2
# Reset: every field back to its default (--respect-keep spares the `keep` ones)
meshfox form reset --canvas examples/forms.canvas.md add-a-purchase purchase
```

`form submit` prints each started block's output and exits non-zero if one of
them fails (`--no-wait` returns at once). An MCP client has the same pair as the
`form_submit` and `form_reset` tools. Unlike `run` with `vars`, both change what the
form shows and what later runs see.

