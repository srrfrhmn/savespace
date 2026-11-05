import initCore from "@savespace/savespace-core-wasm";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing #root element");
}

// Load savespace-core (WASM) once, before the first render. Every money value
// on screen goes through it (design doc §8.4).
await initCore();

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
