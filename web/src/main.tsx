import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ReactFlowProvider } from "@xyflow/react";
import App from "./App";
import "@fontsource/fira-code/400.css";
import "@fontsource/fira-code/500.css";
import "@fontsource/fira-code/600.css";
import "@fontsource/fira-code/700.css";
import "./index.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {/* Explicit, app-wide, rather than relying on `<ReactFlow>`'s own
     * implicit self-wrapping provider (which only covers its own
     * subtree — internally rendered nodes like MeshNode, not anything
     * `App` renders as a sibling, e.g. NodeExpandPanel's portal). A node's
     * "jump to dependency" button (`useReactFlow()`, to pan/center the
     * target's node) needs this same context to keep working once that
     * body is rendered there too, not just inline in the canvas. */}
    <ReactFlowProvider>
      <App />
    </ReactFlowProvider>
  </StrictMode>,
);
