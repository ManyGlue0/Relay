import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { useStore } from "./lib/store";
import "./styles.css";

// No browser chrome: no default context menu, no zoom, reload or print shortcuts.
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement;
  if (!t.closest("input, textarea, .md, .bubble")) e.preventDefault();
});
window.addEventListener("keydown", (e) => {
  if (e.ctrlKey && !e.shiftKey && ["p", "r", "g", "j", "u", "s"].includes(e.key.toLowerCase())) e.preventDefault();
  if (e.key === "F5" || e.key === "F3" || e.key === "F7" || (e.key === "F12" && !import.meta.env.DEV)) e.preventDefault();
});
window.addEventListener("wheel", (e) => e.ctrlKey && e.preventDefault(), { passive: false });

if (import.meta.env.DEV) (window as unknown as { relay: typeof useStore }).relay = useStore;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </StrictMode>,
);
