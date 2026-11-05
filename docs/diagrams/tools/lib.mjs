// Minimal Excalidraw scene builder.
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

export const C = {
  ink: "#1e1e1e",
  muted: "#868e96",
  blue: "#a5d8ff",
  green: "#b2f2bb",
  yellow: "#ffec99",
  red: "#ffc9c9",
  violet: "#d0bfff",
  gray: "#e9ecef",
  orange: "#ffd8a8",
  teal: "#96f2d7",
  pink: "#fcc2d7",
  white: "#ffffff",
  zone: "#f8f9fa",
};

const FONT = 5; // Excalifont
let seedCounter = 1000;
const seed = () => (seedCounter += 7919) % 2147483647;

function base(type, id, x, y, w, h, o = {}) {
  return {
    id,
    type,
    x,
    y,
    width: w,
    height: h,
    angle: 0,
    strokeColor: o.stroke ?? C.ink,
    backgroundColor: o.bg ?? "transparent",
    fillStyle: "solid",
    strokeWidth: o.strokeWidth ?? 2,
    strokeStyle: o.dashed ? "dashed" : "solid",
    roughness: o.roughness ?? 1,
    opacity: 100,
    groupIds: [],
    frameId: null,
    index: null,
    roundness: o.roundness === undefined ? null : o.roundness,
    seed: seed(),
    version: 1,
    versionNonce: seed(),
    isDeleted: false,
    boundElements: [],
    updated: 1762214400000,
    link: null,
    locked: false,
  };
}

function measure(text, size) {
  const lines = String(text).split("\n");
  const longest = Math.max(...lines.map((l) => l.length));
  return { w: Math.ceil(longest * size * 0.56) + 4, h: Math.ceil(lines.length * size * 1.25) };
}

export class Diagram {
  constructor(name) {
    this.name = name;
    this.els = [];
    this.byId = new Map();
    this.n = 0;
  }
  uid(p) {
    return `${p}-${++this.n}`;
  }
  push(el) {
    this.els.push(el);
    this.byId.set(el.id, el);
    return el;
  }

  textEl(id, x, y, text, o = {}) {
    const size = o.size ?? 18;
    const m = measure(text, size);
    const t = base("text", id, x, y, m.w, m.h, { stroke: o.color ?? C.ink, roughness: 0 });
    Object.assign(t, {
      text,
      originalText: text,
      fontSize: size,
      fontFamily: FONT,
      textAlign: o.align ?? "left",
      verticalAlign: o.valign ?? "top",
      containerId: o.containerId ?? null,
      lineHeight: 1.25,
      autoResize: true,
    });
    return t;
  }

  text(x, y, text, o = {}) {
    return this.push(this.textEl(this.uid("t"), x, y, text, o));
  }

  title(x, y, text) {
    return this.text(x, y, text, { size: 28 });
  }

  box(id, x, y, w, h, label, o = {}) {
    const shape = o.shape ?? "rectangle";
    const b = base(shape, id, x, y, w, h, {
      bg: o.bg ?? C.white,
      dashed: o.dashed,
      stroke: o.stroke,
      roundness: shape === "rectangle" ? { type: 3 } : shape === "diamond" ? { type: 2 } : null,
    });
    this.push(b);
    if (label) {
      const size = o.size ?? 18;
      const align = o.align ?? "center";
      const m = measure(label, size);
      const tx = align === "left" ? x + 12 : x + (w - m.w) / 2;
      const valign = o.valign ?? "middle";
      const ty = valign === "top" ? y + 10 : y + (h - m.h) / 2;
      const t = this.textEl(`${id}-label`, tx, ty, label, {
        size,
        align,
        valign,
        containerId: id,
      });
      b.boundElements.push({ type: "text", id: t.id });
      this.push(t);
    }
    return b;
  }

  // Dashed grouping rectangle with a free title at its top-left corner.
  zone(id, x, y, w, h, title, o = {}) {
    const z = base("rectangle", id, x, y, w, h, {
      bg: o.bg ?? C.zone,
      dashed: true,
      stroke: o.stroke ?? C.muted,
      strokeWidth: 1,
      roundness: { type: 3 },
    });
    this.push(z);
    if (title) this.text(x + 14, y + 10, title, { size: o.size ?? 20, color: o.color ?? "#495057" });
    return z;
  }

  side(b, s) {
    switch (s) {
      case "l": return [b.x, b.y + b.height / 2];
      case "r": return [b.x + b.width, b.y + b.height / 2];
      case "t": return [b.x + b.width / 2, b.y];
      case "b": return [b.x + b.width / 2, b.y + b.height];
    }
  }

  autoSides(a, b) {
    if (b.x >= a.x + a.width) return ["r", "l"];
    if (b.x + b.width <= a.x) return ["l", "r"];
    if (b.y >= a.y + a.height) return ["b", "t"];
    return ["t", "b"];
  }

  // Arrow from box to box, bound at both ends.
  arrow(from, to, label, o = {}) {
    const a = this.byId.get(from);
    const b = this.byId.get(to);
    if (!a || !b) throw new Error(`${this.name}: missing ${from} or ${to}`);
    const [sa, sb] = o.sides ?? this.autoSides(a, b);
    const [x1, y1] = this.side(a, sa).map((v, i) => v + (o.offset?.[0]?.[i] ?? 0));
    const [x2, y2] = this.side(b, sb).map((v, i) => v + (o.offset?.[1]?.[i] ?? 0));
    const id = this.uid("a");
    const via = o.via ?? [];
    const pts = [[0, 0], ...via.map(([vx, vy]) => [vx - x1, vy - y1]), [x2 - x1, y2 - y1]];
    const el = this.arrowEl(id, x1, y1, pts, o);
    el.startBinding = { elementId: from, focus: 0, gap: 4 };
    el.endBinding = { elementId: to, focus: 0, gap: 4 };
    a.boundElements.push({ type: "arrow", id });
    b.boundElements.push({ type: "arrow", id });
    this.push(el);
    if (label) this.arrowLabel(el, label, o);
    return el;
  }

  // Free arrow between two points (used for sequence diagrams).
  freeArrow(x1, y1, x2, y2, label, o = {}) {
    const el = this.arrowEl(this.uid("a"), x1, y1, [[0, 0], [x2 - x1, y2 - y1]], o);
    this.push(el);
    if (label) this.arrowLabel(el, label, { ...o, above: true });
    return el;
  }

  arrowEl(id, x, y, pts, o) {
    const xs = pts.map((p) => p[0]);
    const ys = pts.map((p) => p[1]);
    const el = base("arrow", id, x, y, Math.max(...xs) - Math.min(...xs), Math.max(...ys) - Math.min(...ys), {
      dashed: o.dashed,
      stroke: o.stroke,
      roundness: pts.length > 2 ? null : { type: 2 },
    });
    Object.assign(el, {
      points: pts,
      lastCommittedPoint: null,
      startBinding: null,
      endBinding: null,
      startArrowhead: o.both ? "arrow" : null,
      endArrowhead: o.none ? null : "arrow",
      elbowed: false,
    });
    return el;
  }

  arrowLabel(el, label, o) {
    const size = o.labelSize ?? 16;
    const m = measure(label, size);
    const last = el.points[el.points.length - 1];
    const mid = el.points.length > 2 ? el.points[Math.floor(el.points.length / 2)] : [last[0] / 2, last[1] / 2];
    const cx = o.labelAt ? o.labelAt[0] : el.x + mid[0];
    const cy = o.labelAt ? o.labelAt[1] : el.y + mid[1];
    if (o.above) {
      // Sequence-style label: free text above the line, not bound.
      this.text(cx - m.w / 2, cy - m.h - 4, label, { size, align: "center" });
      return;
    }
    const t = this.textEl(`${el.id}-label`, cx - m.w / 2, cy - m.h / 2, label, {
      size,
      align: "center",
      valign: "middle",
      containerId: el.id,
    });
    el.boundElements.push({ type: "text", id: t.id });
    this.push(t);
  }

  line(x1, y1, x2, y2, o = {}) {
    const el = base("line", this.uid("l"), x1, y1, Math.abs(x2 - x1), Math.abs(y2 - y1), {
      dashed: o.dashed,
      stroke: o.stroke ?? C.muted,
      strokeWidth: o.strokeWidth ?? 1,
      roundness: null,
    });
    Object.assign(el, {
      points: [[0, 0], [x2 - x1, y2 - y1]],
      lastCommittedPoint: null,
      startBinding: null,
      endBinding: null,
      startArrowhead: null,
      endArrowhead: null,
    });
    return this.push(el);
  }

  scene() {
    return {
      type: "excalidraw",
      version: 2,
      source: "https://excalidraw.com",
      elements: this.els,
      appState: { gridSize: 20, viewBackgroundColor: "#ffffff" },
      files: {},
    };
  }

  save(dir) {
    mkdirSync(dir, { recursive: true });
    const p = join(dir, `${this.name}.excalidraw`);
    writeFileSync(p, JSON.stringify(this.scene(), null, 2) + "\n");
    return p;
  }
}
