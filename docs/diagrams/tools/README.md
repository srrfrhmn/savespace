# Diagram tools

These scripts build the Excalidraw diagrams in `docs/diagrams/` and export them to SVG.

| File | Purpose |
| --- | --- |
| `lib.mjs` | A small Excalidraw scene builder: boxes, zones, bound arrows, text. |
| `build.mjs` | The diagram definitions. Writes `NN-name.excalidraw`. |
| `export.mjs` | Exports each `.excalidraw` file to `.svg` with the official `exportToSvg`, in a headless browser. |

## Rebuild

```sh
pnpm diagrams
```

The export uses an installed Edge (Windows) or Chrome. Set `DIAGRAM_BROWSER=chrome` or `msedge` to choose. It loads `@excalidraw/excalidraw` from esm.sh, so it needs network access.

## Edit a diagram

- Small change: open the `.excalidraw` file in excalidraw.com, edit, save, and export the SVG. Then update `build.mjs` to match, or the next rebuild overwrites your change.
- Structural change: edit `build.mjs`, then run `pnpm diagrams`.
