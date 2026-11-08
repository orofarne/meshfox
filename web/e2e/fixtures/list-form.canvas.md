<!-- meshfox:canvas -->
# List Form Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright list-form suite (`web/e2e/list-form.spec.ts`):
a `list` variable with `choices` is a set of checkboxes, one without is a
textarea with one element per line; both travel as the stored JSON array.

## Lists
<!-- meshfox:node id="lists" -->

<!-- meshfox:var name="LIST_REGIONS" type="list" choices="us,eu,asia" default='["eu"]' -->
<!-- meshfox:var name="LIST_TAGS" type="list" default='[]' -->

```form name="pick" send="Apply"
field var="LIST_REGIONS" label="Regions"
field var="LIST_TAGS" label="Tags"
```

```bash name="show" env="LIST_REGIONS|comma,LIST_TAGS|space" autorun
echo "regions=$LIST_REGIONS"
echo "tags=$LIST_TAGS"
```
