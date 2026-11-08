<!-- meshfox:canvas -->
# Form Step Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright form-step suite (`web/e2e/form-step.spec.ts`)
— a `form` fence as a step of a run chain (see SPEC.md's "Form fences"): the
run parks on it until it is sent or cancelled.

## Flow
<!-- meshfox:node id="flow" -->

<!-- meshfox:var name="STEP_ANSWER" prompt="Answer" default="" -->

```bash name="prep"
echo prep-done
```

```form name="review" deps="prep" send="Continue" cancel="Stop the run"
text "Check the file, then continue"
field var="STEP_ANSWER" label="Answer"
```

```bash name="finish" deps="review" env="STEP_ANSWER"
echo "result-is-$STEP_ANSWER"
```
