import { useCallback, useRef, useState } from "react";
import type { RunEvent } from "./api";

export type ActivityAddress = { nodeId: string; block: string };
export type ActivityPhase = "preparing" | "running" | "between" | "interactive" | "completed" | "failed" | "killed" | "unknown";
export interface RunActivity {
  key: string;
  target: ActivityAddress;
  current?: ActivityAddress;
  phase: ActivityPhase;
  startedAt: number;
  stepStartedAt?: number;
  endedAt?: number;
  runId?: string;
  exitCode?: number;
  message?: string;
  addresses: string[];
}
export const activityIsLive = (a: RunActivity) => a.endedAt === undefined;

/** Chain lifetime and process lifetime are separate: StepEnd clears the
 * process clock, while only Done (or an abnormal end) settles the chain. */
export function activityEvent(a: RunActivity, event: RunEvent, now: number): RunActivity {
  if (!activityIsLive(a)) return a;
  switch (event.type) {
    case "started": return { ...a, runId: event.runId };
    case "step-start": return { ...a, phase: "running", current: event, stepStartedAt: now,
      addresses: [...new Set([...a.addresses, `${event.nodeId}::${event.block}`])] };
    case "tty-start": return { ...a, phase: "interactive", current: event };
    case "step-skipped":
    case "step-end":
    case "service-started": return { ...a, phase: "between", current: undefined, stepStartedAt: undefined };
    case "done": return { ...a, phase: event.exitCode === 0 ? "completed" : "failed", exitCode: event.exitCode,
      current: undefined, stepStartedAt: undefined, endedAt: now };
    case "killed": return { ...a, phase: "killed", current: undefined, stepStartedAt: undefined, endedAt: now };
    case "error": return { ...a, phase: "failed", message: event.message, endedAt: now };
    case "lock-conflict": return { ...a, phase: "failed", message: "Run blocked by another process", endedAt: now };
    default: return a;
  }
}

export function useRunActivity() {
  const [activities, setActivities] = useState<RunActivity[]>([]);
  const serial = useRef(0);
  const begin = useCallback((target: ActivityAddress, addresses: string[]) => {
    const key = `local:${++serial.current}`;
    setActivities(previous => [...previous.filter(activityIsLive), {
      key, target, addresses, phase: "preparing", startedAt: Date.now(),
    }]);
    return key;
  }, []);
  const event = useCallback((key: string, value: RunEvent) => {
    const now = Date.now();
    setActivities(previous => previous.map(a => a.key === key ? activityEvent(a, value, now) : a));
  }, []);
  const close = useCallback((key: string, message?: string) => {
    setActivities(previous => previous.map(a => a.key === key && activityIsLive(a)
      ? { ...a, phase: "unknown", message: message ?? "Connection ended before the run result arrived", endedAt: Date.now() }
      : a));
  }, []);
  const clear = useCallback(() => setActivities(previous => previous.filter(activityIsLive)), []);
  return { activities, begin, event, close, clear };
}
