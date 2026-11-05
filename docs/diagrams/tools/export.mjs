// Export .excalidraw files to .svg with the official exportToSvg in a headless browser.
import { chromium } from "playwright-core";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Default input and output: docs/diagrams.
const DIR = process.argv[2] ?? fileURLToPath(new URL("..", import.meta.url));
// Uses an installed browser, so no browser download is needed.
// Set DIAGRAM_BROWSER to "chrome" or "msedge".
const CHANNEL = process.env.DIAGRAM_BROWSER ?? (process.platform === "win32" ? "msedge" : "chrome");
const files = readdirSync(DIR).filter((f) => f.endsWith(".excalidraw"));

const browser = await chromium.launch({ channel: CHANNEL, headless: true });
const page = await browser.newPage();
page.on("console", (m) => console.log("[page]", m.text()));
await page.goto("https://esm.sh/");
await page.evaluate(async () => {
  window.EXCALIDRAW_ASSET_PATH = "https://esm.sh/@excalidraw/excalidraw@0.18.0/dist/prod/";
  const mod = await import("https://esm.sh/@excalidraw/excalidraw@0.18.0?deps=react@19.1.0,react-dom@19.1.0");
  window.__ex = mod;
});

for (const f of files) {
  const scene = JSON.parse(readFileSync(join(DIR, f), "utf8"));
  const svg = await page.evaluate(async (scene) => {
    const { exportToSvg, restore } = window.__ex;
    const r = restore(scene, null, null);
    const el = await exportToSvg({
      elements: r.elements,
      appState: { ...r.appState, exportBackground: true, viewBackgroundColor: "#ffffff", exportWithDarkMode: false, exportEmbedScene: true },
      files: r.files,
      exportPadding: 24,
    });
    return el.outerHTML;
  }, scene);
  const out = join(DIR, f.replace(/\.excalidraw$/, ".svg"));
  writeFileSync(out, svg);
  console.log(out, Math.round(svg.length / 1024) + " KB");
}
await browser.close();
