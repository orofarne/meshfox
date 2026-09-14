<!-- meshfox:canvas -->
# Reload Live Run Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright reload-live-run suite
(`web/e2e/reload-live-run.spec.ts`) — a long-running block's live output
is purely client-side React state (`MeshNodeData.liveBlocks`), lost the
instant a tab reloads; this checks App.tsx's own reconciliation effect
(`reconciledActiveRunsRef`, right after the canvas first loads: `GET
/api/runs` for every block this server process still knows about, then
`GET /api/run/subscribe` to replay its buffered backlog and resume the
live tail) actually restores both halves — lines already printed *before*
the reload, and ones the process goes on to print *after* it — rather
than losing the first or getting stuck on the second.

```bash name="long-task"
for i in 1 2 3 4 5 6; do
  echo "line $i"
  sleep 1
done
echo done
```
