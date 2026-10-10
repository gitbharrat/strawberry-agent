// Upgrades the markdown renderer's static `.md-design` blocks into live
// editors, right in the transcript.
//
// The renderer emits plain HTML (it must — it is a string pipeline), so the
// interactivity is attached afterwards at the DOM level: a colour input inside
// every swatch, grid drag/resize/rename on wireframe areas, and a "sketch"
// hand-off to the scratchpad. Every committed edit is expressed as a mutation
// of the design JSON, which the caller writes back to wherever this block
// lives (a chat message, a graph artifact) — the DOM itself is then re-rendered
// from the new content, so there is exactly one source of truth.

import type { DesignJson } from "./meg";

export type DesignMutator = (d: DesignJson) => void;

export interface DesignHandlers {
  /** Commit one structural edit. Absent → the block stays view-only. */
  onEdit?: (mutate: DesignMutator) => void;
  /** Open the sketch scratchpad seeded with this design's wireframe. */
  onSketch?: (designJson: string) => void;
}

export function enhanceDesignBlocks(host: HTMLElement, handlers: DesignHandlers): void {
  const blocks = host.querySelectorAll<HTMLElement>(".md-design[data-design]:not([data-live])");
  for (const block of blocks) {
    block.dataset.live = "1";
    const json = block.dataset.design ?? "";
    if (handlers.onSketch && block.querySelector(".md-design-wireframe")) {
      addSketchButton(block, json, handlers.onSketch);
    }
    if (!handlers.onEdit) continue;
    block.dataset.editable = "1";
    wireSwatches(block, handlers.onEdit);
    wireAreas(block, handlers.onEdit);
  }
}

function addSketchButton(block: HTMLElement, json: string, onSketch: (json: string) => void): void {
  const head = block.querySelector(".md-design-head");
  if (!head) return;
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "md-design-sketch";
  btn.textContent = "✎ sketch";
  btn.title = "Open this wireframe in the scratchpad — redraw it, annotate it, send it back to the agent";
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    onSketch(json);
  });
  head.insertBefore(btn, head.querySelector(".md-design-tag"));
}

// ------------------------------------------------------------------ swatches

function wireSwatches(block: HTMLElement, onEdit: (m: DesignMutator) => void): void {
  for (const swatch of block.querySelectorAll<HTMLElement>(".md-design-swatch[data-i]")) {
    const index = Number(swatch.dataset.i);
    if (!Number.isFinite(index)) continue;
    const input = document.createElement("input");
    input.type = "color";
    input.className = "md-design-pick";
    input.value = normalizeHexInput(swatch.dataset.hex ?? "#888888");
    input.title = "Click to change this colour";
    input.addEventListener("click", (e) => e.stopPropagation());
    input.addEventListener("change", () => {
      const hex = input.value;
      onEdit((d) => {
        const c = d.theme?.colors?.[index];
        if (!c) return;
        c.hex = hex;
        // Some models emit `color` instead of `hex`; drop it so the two
        // spellings cannot disagree after an edit.
        if ("color" in c) delete c.color;
      });
    });
    swatch.appendChild(input);
  }
}

/** `<input type=color>` accepts only #rrggbb. */
function normalizeHexInput(hex: string): string {
  if (/^#[0-9a-f]{6}$/i.test(hex)) return hex;
  if (/^#[0-9a-f]{3}$/i.test(hex)) {
    return `#${hex[1]}${hex[1]}${hex[2]}${hex[2]}${hex[3]}${hex[3]}`;
  }
  if (/^#[0-9a-f]{8}$/i.test(hex)) return hex.slice(0, 7);
  return "#888888";
}

// ------------------------------------------------------------------ wireframe

function wireAreas(block: HTMLElement, onEdit: (m: DesignMutator) => void): void {
  const frame = block.querySelector<HTMLElement>(".md-design-wireframe");
  if (!frame) return;
  const cols = Number(frame.dataset.cols) || 12;
  const rows = Number(frame.dataset.rows) || 8;

  for (const area of frame.querySelectorAll<HTMLElement>(".md-design-area[data-i]")) {
    const index = Number(area.dataset.i);
    if (!Number.isFinite(index)) continue;

    const handle = document.createElement("span");
    handle.className = "md-design-handle";
    handle.title = "Drag to resize";
    area.appendChild(handle);

    area.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      const resize = e.target === handle;
      startAreaDrag(e, { frame, area, index, cols, rows, resize, onEdit });
    });
    area.addEventListener("dblclick", (e) => {
      e.stopPropagation();
      startRename(area, index, onEdit);
    });
  }
}

interface AreaDrag {
  frame: HTMLElement;
  area: HTMLElement;
  index: number;
  cols: number;
  rows: number;
  resize: boolean;
  onEdit: (m: DesignMutator) => void;
}

function startAreaDrag(e: PointerEvent, ctx: AreaDrag): void {
  e.preventDefault();
  e.stopPropagation();
  const rect = ctx.frame.getBoundingClientRect();
  const cellW = Math.max(1, rect.width / ctx.cols);
  const cellH = Math.max(1, rect.height / ctx.rows);
  const sx = e.clientX;
  const sy = e.clientY;
  const orig = {
    x: Number(ctx.area.dataset.x) || 0,
    y: Number(ctx.area.dataset.y) || 0,
    w: Number(ctx.area.dataset.w) || 1,
    h: Number(ctx.area.dataset.h) || 1,
  };
  let cur = { ...orig };
  let moved = false;

  const apply = () => {
    ctx.area.style.gridColumn = `${cur.x + 1} / span ${cur.w}`;
    ctx.area.style.gridRow = `${cur.y + 1} / span ${cur.h}`;
  };
  const onMove = (ev: PointerEvent) => {
    const dx = Math.round((ev.clientX - sx) / cellW);
    const dy = Math.round((ev.clientY - sy) / cellH);
    if (dx !== 0 || dy !== 0) moved = true;
    cur = ctx.resize
      ? {
          x: orig.x,
          y: orig.y,
          w: Math.max(1, Math.min(ctx.cols - orig.x, orig.w + dx)),
          h: Math.max(1, Math.min(ctx.rows - orig.y, orig.h + dy)),
        }
      : {
          x: Math.max(0, Math.min(ctx.cols - orig.w, orig.x + dx)),
          y: Math.max(0, Math.min(ctx.rows - orig.h, orig.y + dy)),
          w: orig.w,
          h: orig.h,
        };
    apply();
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    ctx.area.dataset.dragging = "";
    if (!moved || (cur.x === orig.x && cur.y === orig.y && cur.w === orig.w && cur.h === orig.h)) return;
    const final = { ...cur };
    ctx.onEdit((d) => {
      const a = d.wireframe?.areas?.[ctx.index];
      if (!a) return;
      a.x = final.x;
      a.y = final.y;
      a.w = final.w;
      a.h = final.h;
    });
  };
  ctx.area.dataset.dragging = "1";
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
}

function startRename(area: HTMLElement, index: number, onEdit: (m: DesignMutator) => void): void {
  if (area.querySelector("input")) return;
  const label = area.querySelector<HTMLElement>(".md-design-area-label");
  const input = document.createElement("input");
  input.className = "md-design-rename";
  input.value = label?.textContent ?? "";
  input.addEventListener("pointerdown", (e) => e.stopPropagation());
  const done = (commit: boolean) => {
    const next = input.value.trim();
    input.remove();
    if (label) label.style.display = "";
    if (!commit || !next || next === label?.textContent) return;
    if (label) label.textContent = next;
    onEdit((d) => {
      const a = d.wireframe?.areas?.[index];
      if (a) a.label = next;
    });
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") done(true);
    if (e.key === "Escape") done(false);
    e.stopPropagation();
  });
  input.addEventListener("blur", () => done(true));
  if (label) label.style.display = "none";
  area.appendChild(input);
  queueMicrotask(() => input.select());
}
