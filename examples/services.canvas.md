<!-- meshfox:canvas -->
# services

Long-lived services, graceful shutdown, and the declarative `@service` interpreter.

Open this canvas with `meshfox view examples/services.canvas.md`. Run one example at a time, then use the Services panel to Stop or Restart it. From the CLI, `meshfox run --canvas examples/services.canvas.md <node> <block>` streams logs until Ctrl-C stops the service.

Stop first disables automatic restarts. It runs the optional stop command (or sends SIGTERM to a foreground process), waits for cleanup, and only then releases ownership. Restart waits for that same shutdown before starting a new generation. A failed or timed-out stop stays visible and can be retried.

The supervisor runs inside the worker. A small identity process keeps the service's PID stable through detached startup and automatic restarts; PID and resource counters refer to that identity for `@service`. Forced worker termination cannot guarantee cleanup of external resources.

## Foreground service
<!-- meshfox:node id="foreground-service" -->

No YAML is required for an ordinary foreground command. Its stdout/stderr become the service log. Stop sends SIGTERM and allows up to 30 seconds for graceful shutdown before SIGKILL.

```bash name="clock" service default
trap 'echo "clock stopped gracefully"; exit 0' TERM INT
while :; do
  date
  sleep 1
done
```

## Declarative foreground service
<!-- meshfox:node id="declarative-foreground-service" -->

`interpreter="@service"` implies `service`. Without `monitor`, `start` remains in the foreground. A nonzero exit triggers automatic restart; a clean exit does not. Delay doubles after repeated failures, capped at 60 seconds, and resets after a minute of healthy operation.

```yaml name="clock" interpreter="@service" default
start: |
  trap 'echo "clock stopped gracefully"; exit 0' TERM INT
  while :; do
    date
    sleep 1
  done
restart_on_fail: true
restart_delay: 2s
stop_timeout: 5s
```

## Docker Compose
<!-- meshfox:node id="docker-compose" -->

This example uses the adjacent `services-data/compose.yaml` and requires Docker with Compose. An explicit project name keeps start, stop, monitor, and check aimed at the same stack. The web service is available at http://localhost:18080.

With `monitor`, start is a one-shot command: it must exit successfully before monitoring begins. Losing the log stream reconnects the monitor, without restarting containers. The optional check determines service failure. Its exit code must be zero when healthy. Checks are sequential and each has a timeout equal to check_interval; they are liveness checks, not a dependency-readiness barrier.

```yaml name="web" interpreter="@service" default
start: docker compose -p meshfox-services-example -f services-data/compose.yaml up --build --wait
stop: docker compose -p meshfox-services-example -f services-data/compose.yaml stop
monitor: docker compose -p meshfox-services-example -f services-data/compose.yaml logs --follow --no-color
check: docker compose -p meshfox-services-example -f services-data/compose.yaml exec -T web wget -q -O /dev/null http://127.0.0.1/
check_interval: 5s
restart_on_fail: true
restart_delay: 2s
stop_timeout: 30s
```

Stop preserves containers and volumes. To remove this example's containers and network afterwards:

```bash name="remove"
docker compose -p meshfox-services-example -f services-data/compose.yaml down
```

