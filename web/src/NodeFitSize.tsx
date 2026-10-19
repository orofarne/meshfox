import { useEffect, useRef, useState, type RefObject } from "react";
import { measureFitSizes, type FitDirection, type NodeSize } from "./contentFit";

const choices: { direction: FitDirection; icon: string; title: string; description: string }[] = [
  { direction: "down", icon: "↧", title: "Down", description: "Keep width" },
  { direction: "right", icon: "⇥", title: "Right", description: "Keep height" },
  { direction: "proportional", icon: "⤢", title: "Proportionally", description: "Keep aspect ratio" },
];
export function NodeFitSize({ nodeRef, onApply, onPreview }: {
  nodeRef: RefObject<HTMLDivElement | null>;
  onApply: (size: NodeSize) => void;
  onPreview: (size: NodeSize | null) => void;
}) {
  const [sizes, setSizes] = useState<Record<FitDirection, NodeSize | null> | null>(null);
  const container = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const close = () => { setSizes(null); onPreview(null); };
  useEffect(() => {
    if (!sizes) return;
    const outside = (event: PointerEvent) => { if (!container.current?.contains(event.target as Node)) close(); };
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape") { event.stopPropagation(); close(); trigger.current?.focus(); } };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape, true);
    return () => { document.removeEventListener("pointerdown", outside); document.removeEventListener("keydown", escape, true); };
  }, [sizes]);
  return <div className="mesh-node-fit" ref={container}>
    <button ref={trigger} type="button" className="mesh-node-icon-button nodrag" aria-label="Fit to content"
      title="Fit to content" aria-haspopup="menu" aria-expanded={!!sizes}
      onClick={() => { if (sizes) close(); else if (nodeRef.current) setSizes(measureFitSizes(nodeRef.current)); }}>⤢</button>
    {sizes && <div className="mesh-node-fit-menu nokey" role="menu" aria-label="Fit to content" tabIndex={-1}
      onKeyDown={event => {
        if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const items = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
        const index = items.indexOf(document.activeElement as HTMLButtonElement);
        const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
          : (index + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
        items[next]?.focus();
      }}
      onMouseLeave={() => onPreview(null)}>
      {choices.map(({ direction, icon, title, description }) => {
        const size = sizes[direction];
        const root = nodeRef.current;
        const unchanged = size && root && size.width <= root.offsetWidth && size.height <= root.offsetHeight;
        return <button key={direction} type="button" role="menuitem" aria-label={title} disabled={!size || !!unchanged}
          title={!size ? "Content cannot fit by growing in this direction" : unchanged ? "Content already fits" : description}
          onMouseEnter={() => onPreview(size && !unchanged ? size : null)}
          onFocus={() => onPreview(size)} onBlur={() => onPreview(null)}
          onClick={() => { if (size) { close(); onApply(size); } }}>
          <span className="mesh-node-fit-direction">{icon}</span><span>{title}<small>{description}</small></span>
          <span className="mesh-node-fit-dimensions">{size ? `${Math.round(size.width)} × ${Math.round(size.height)}` : "—"}</span>
        </button>;
      })}
    </div>}
  </div>;
}
