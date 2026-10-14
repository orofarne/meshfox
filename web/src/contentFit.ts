export interface NodeSize { width: number; height: number }
export type FitDirection = "down" | "right" | "proportional";

/** Find the smallest enlargement that fits. Width-only fitting can be
 * impossible (e.g. too many explicit lines for the retained height). */
export function findFitSize(current: NodeSize, direction: FitDirection,
  fits: (size: NodeSize) => boolean): NodeSize | null {
  if (fits(current)) return current;
  const candidate = (scale: number): NodeSize => ({
    width: direction === "down" ? current.width : Math.ceil(current.width * scale / 4) * 4,
    height: direction === "right" ? current.height : Math.ceil(current.height * scale / 4) * 4,
  });
  let low = 1, high = 1;
  while (high < 64) {
    high *= 2;
    if (fits(candidate(high))) break;
    low = high;
  }
  if (!fits(candidate(high))) return null;
  for (let i = 0; i < 16; i++) {
    const middle = (low + high) / 2;
    if (fits(candidate(middle))) high = middle;
    else low = middle;
  }
  return candidate(high);
}

/** Measure an inert copy, so trying widths never moves the actual node or
 * changes its scroll position. Retain inherited styles, including zoom-free
 * font sizes; all returned dimensions are in canvas coordinates. */
export function measureFitSizes(root: HTMLElement): Record<FitDirection, NodeSize | null> {
  const current = { width: root.offsetWidth, height: root.offsetHeight };
  const host = document.createElement("div");
  host.className = "react-flow__node";
  Object.assign(host.style, { position: "fixed", left: "-100000px", top: "0", visibility: "hidden", pointerEvents: "none", transform: "none" });
  host.setAttribute("inert", "");
  const clone = root.cloneNode(true) as HTMLElement;
  clone.querySelectorAll(".react-flow__handle, .react-flow__resize-control, .mesh-node-fit-preview").forEach(el => el.remove());
  Object.assign(clone.style, { width: "100%", maxHeight: "none" });
  const centered = clone.querySelector<HTMLElement>(".mesh-node-title-centered");
  if (centered) { centered.style.removeProperty("font-size"); centered.removeAttribute("data-autofit"); }
  host.append(clone);
  // Keep theme variables and font inherited from this view (also works inside
  // the group expansion panel). The host itself remains outside normal flow.
  root.parentElement!.append(host);
  const fits = ({ width, height }: NodeSize) => {
    host.style.width = `${width}px`;
    clone.style.height = `${height}px`;
    const elements = [clone, ...clone.querySelectorAll<HTMLElement>(
      ".mesh-node-clip, .mesh-node-body, .mesh-file-code-preview, .mesh-node-title-centered, .mesh-node-title-text, pre, table")];
    return elements.every(el => el.scrollHeight <= el.clientHeight + 1 && el.scrollWidth <= el.clientWidth + 1);
  };
  try {
    return { down: findFitSize(current, "down", fits), right: findFitSize(current, "right", fits),
      proportional: findFitSize(current, "proportional", fits) };
  } finally { host.remove(); }
}
