import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { reloadForNewBundle } from "./lib/stale-bundle";
import "./index.css";

// Vite reports a failed chunk fetch here before the import rejects; a tab
// left open across a deploy reloads onto the new bundle (lib/stale-bundle).
window.addEventListener("vite:preloadError", (e) => {
  if (reloadForNewBundle()) e.preventDefault();
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
