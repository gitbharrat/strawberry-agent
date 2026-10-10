// The sketch scratchpad: an Excalidraw-flavoured drawing surface for
// wireframes.
//
// Open it blank, or seed it from a design block's wireframe — the areas come
// in as labelled rectangles you can move, redraw and annotate with pen,
// shapes, arrows and text. "Send to agent" ships the drawing into the chat as
// a real image attachment (vision models see the pixels) together with a
// coordinate summary (every model reads the structure), so "make it look like
// THIS" becomes something the agent can actually act on.
//
// The canvas is a fixed 1200×800 logical space rendered as SVG — crisp at any
// pane size, and serialisable straight to PNG without a screenshot API.

import { batch, createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { createStore, produce, reconcile, unwrap } from "solid-js/store";
import { Check, Trash, Undo } from "./Icons";
import { api } from "~/lib/ipc";
import { megState } from "~/lib/megState";
import { getSession } from "~/lib/session";
import { store, toast } from "~/lib/store";
import { errText } from "~/lib/err";
import type { DesignJson } from "~/lib/meg";

const W = 1200;
const H = 800;

export interface SketchShape {
  id: string;
  kind: "pen" | "rect" | "ellipse" | "arrow" | "text" | "image";
  color: string;
  /** Bounds. For arrows: from (x,y) to (x+w,y+h) — w/h may be negative. */
  x: number;
  y: number;
  w: number;
  h: number;
  points?: { x: number; y: number }[];
  text?: string;
  /** Pasted/dropped image as a data URL — annotations draw over it. */
  href?: string;
}

type Tool = "select" | "pen" | "rect" | "ellipse" | "arrow" | "text";

const PALETTE = ["#8c8c8c", "#e5484d", "#d99a34", "#3fd082", "#6892c0", "#9585d6"];

const sid = () => Math.random().toString(36).slice(2, 9);

// ---- cross-module entry points -------------------------------------------

const [seed, setSeed] = createSignal<{ shapes: SketchShape[]; palette: string[] } | null>(null);

/** A graph sketch node this pad is drawing FOR: Save stores the drawing as
 * that node's pinned artifact, so runs consume it without ever pausing. */
export interface SketchNodeTarget {
  graphId: string;
  nodeId: string;
  label: string;
}
const [target, setTarget] = createSignal<SketchNodeTarget | null>(null);

/** Opens the scratchpad pane, optionally replacing its contents. */
export function openSketch(): void {
  setTarget(null);
  store.openPane({ kind: "sketch" });
}

/** Queues a seed built from a design block's wireframe: every area becomes a
 * labelled rectangle, and the theme's colours join the palette. */
function seedFromDesign(designJson: string): void {
  try {
    const d = JSON.parse(designJson) as DesignJson;
    const wf = d.wireframe;
    const cols = Math.max(1, Math.round(wf?.cols ?? 12));
    const rows = Math.max(1, Math.round(wf?.rows ?? 8));
    const ox = 60;
    const oy = 60;
    const iw = W - ox * 2;
    const ih = H - oy * 2;
    const shapes: SketchShape[] = [];
    for (const a of (wf?.areas ?? []).slice(0, 40)) {
      const x = Math.max(0, Math.min(cols, Number(a.x) || 0));
      const y = Math.max(0, Math.min(rows, Number(a.y) || 0));
      const w = Math.max(1, Math.min(cols - x, Number(a.w) || 1));
      const h = Math.max(1, Math.min(rows - y, Number(a.h) || 1));
      shapes.push({
        id: sid(),
        kind: "rect",
        color: a.role === "cta" || a.role === "hero" ? "#d99a34" : "#6892c0",
        x: Math.round(ox + (x / cols) * iw),
        y: Math.round(oy + (y / rows) * ih),
        w: Math.round((w / cols) * iw),
        h: Math.round((h / rows) * ih),
        text: typeof a.label === "string" ? a.label : (a.role ?? ""),
      });
    }
    const palette = (d.theme?.colors ?? [])
      .map((c) => c.hex ?? c.color)
      .filter((h): h is string => typeof h === "string" && /^#[0-9a-fA-F]{6}$/.test(h))
      .slice(0, 6);
    setSeed({ shapes, palette });
  } catch {
    setSeed(null);
  }
}

/** Opens the scratchpad seeded with a design block's wireframe. */
export function openSketchFromDesign(designJson: string): void {
  setTarget(null);
  seedFromDesign(designJson);
  store.openPane({ kind: "sketch" });
}

/** Opens the scratchpad FOR a graph sketch node — the node's own UI. When an
 * upstream design's wireframe is handed in, the canvas starts from it (unless
 * a drawing for this chat already exists). Save stores the drawing on the
 * node, pinned, so runs use it without pausing. */
export function openSketchForNode(node: SketchNodeTarget, designJson?: string | null): void {
  setTarget(node);
  if (designJson) seedFromDesign(designJson);
  store.openPane({ kind: "sketch" });
}

// ---- the pane -------------------------------------------------------------

export function SketchPad() {
  const chatId = () => store.activeChatId();
  const [shapes, setShapes] = createStore<SketchShape[]>([]);
  const [tool, setTool] = createSignal<Tool>("select");
  const [color, setColor] = createSignal(PALETTE[4]!);
  const [selected, setSelected] = createSignal<string | null>(null);
  const [extraColors, setExtraColors] = createSignal<string[]>([]);
  const [note, setNote] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [editing, setEditing] = createSignal<{ id: string; sx: number; sy: number } | null>(null);
  let undoStack: SketchShape[][] = [];
  let svgEl!: SVGSVGElement;
  let wrapEl!: HTMLDivElement;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let loadedFor: string | null = null;

  const storageKey = (cid: string) => `sa.sketch.${cid}`;

  function persist() {
    const cid = chatId();
    if (!cid) return;
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      try {
        localStorage.setItem(storageKey(cid), JSON.stringify(unwrap(shapes)));
      } catch {
        /* private mode */
      }
    }, 400);
  }

  // Load per chat; consume a pending seed whichever chat is active.
  createEffect(
    on([chatId, seed], ([cid, s]) => {
      if (cid && cid !== loadedFor) {
        loadedFor = cid;
        undoStack = [];
        setSelected(null);
        try {
          const raw = localStorage.getItem(storageKey(cid));
          setShapes(reconcile(raw ? (JSON.parse(raw) as SketchShape[]) : []));
        } catch {
          setShapes(reconcile([]));
        }
      }
      if (s) {
        setSeed(null);
        // Opening a sketch NODE that already has a drawing in this chat keeps
        // the drawing — the wireframe seed only fills an empty canvas.
        const keepDrawing = target() !== null && shapes.length > 0;
        pushUndo();
        batch(() => {
          if (!keepDrawing) setShapes(reconcile(s.shapes));
          setExtraColors(s.palette);
        });
        if (!keepDrawing) persist();
      }
    }),
  );
  onCleanup(() => {
    if (saveTimer) clearTimeout(saveTimer);
  });

  function pushUndo() {
    undoStack.push(structuredClone(unwrap(shapes)));
    if (undoStack.length > 50) undoStack.shift();
  }
  function undo() {
    const prev = undoStack.pop();
    if (!prev) return;
    setShapes(reconcile(prev));
    setSelected(null);
    persist();
  }

  // ---- geometry

  function toLogical(e: PointerEvent | MouseEvent): { x: number; y: number } {
    // `meet` letterboxes the viewBox inside the element; map through the real
    // content box or every stroke lands beside the cursor.
    const r = svgEl.getBoundingClientRect();
    const scale = Math.min(r.width / W, r.height / H) || 1;
    const ox = (r.width - W * scale) / 2;
    const oy = (r.height - H * scale) / 2;
    return {
      x: Math.max(0, Math.min(W, (e.clientX - r.left - ox) / scale)),
      y: Math.max(0, Math.min(H, (e.clientY - r.top - oy) / scale)),
    };
  }

  function bboxOf(s: SketchShape): { x: number; y: number; w: number; h: number } {
    if (s.kind === "pen" && s.points?.length) {
      const xs = s.points.map((p) => p.x);
      const ys = s.points.map((p) => p.y);
      const x = Math.min(...xs);
      const y = Math.min(...ys);
      return { x, y, w: Math.max(...xs) - x, h: Math.max(...ys) - y };
    }
    const x = Math.min(s.x, s.x + s.w);
    const y = Math.min(s.y, s.y + s.h);
    return { x, y, w: Math.abs(s.w), h: Math.abs(s.h) };
  }

  function hitTest(p: { x: number; y: number }): SketchShape | null {
    for (let i = shapes.length - 1; i >= 0; i--) {
      const s = shapes[i]!;
      const b = bboxOf(s);
      const pad = 8;
      if (p.x >= b.x - pad && p.x <= b.x + b.w + pad && p.y >= b.y - pad && p.y <= b.y + b.h + pad) {
        return s;
      }
    }
    return null;
  }

  // ---- drawing interactions

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if (editing()) return; // The text input owns the pointer until committed.
    const p = toLogical(e);
    const t = tool();
    (e.currentTarget as Element).setPointerCapture(e.pointerId);

    if (t === "select") {
      const hit = hitTest(p);
      setSelected(hit?.id ?? null);
      if (!hit) return;
      pushUndo();
      const orig = structuredClone(unwrap(shapes.find((s) => s.id === hit.id)!));
      const move = (ev: PointerEvent) => {
        const q = toLogical(ev);
        const dx = q.x - p.x;
        const dy = q.y - p.y;
        setShapes(
          (s) => s.id === hit.id,
          (s) => ({
            ...s,
            x: orig.x + dx,
            y: orig.y + dy,
            points: orig.points?.map((pt) => ({ x: pt.x + dx, y: pt.y + dy })),
          }),
        );
      };
      const up = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        persist();
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
      return;
    }

    if (t === "text") {
      pushUndo();
      const shape: SketchShape = {
        id: sid(),
        kind: "text",
        color: color(),
        x: p.x,
        y: p.y,
        w: 0,
        h: 0,
        text: "",
      };
      setShapes(shapes.length, shape);
      openTextEditor(shape.id, e.clientX, e.clientY);
      return;
    }

    pushUndo();
    const shape: SketchShape =
      t === "pen"
        ? { id: sid(), kind: "pen", color: color(), x: p.x, y: p.y, w: 0, h: 0, points: [p] }
        : { id: sid(), kind: t, color: color(), x: p.x, y: p.y, w: 0, h: 0 };
    setShapes(shapes.length, shape);
    const idx = shapes.length - 1;
    let lastPen = 0;
    const move = (ev: PointerEvent) => {
      const q = toLogical(ev);
      if (t === "pen") {
        // ~120Hz pointers produce absurd point counts; 15ms thinning is invisible.
        const now = performance.now();
        if (now - lastPen < 15) return;
        lastPen = now;
        setShapes(idx, "points", (pts) => [...(pts ?? []), q]);
      } else {
        setShapes(idx, { w: q.x - shape.x, h: q.y - shape.y });
      }
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const s = shapes[idx];
      // Discard accidental dots (except pen taps, which are still marks).
      if (s && s.kind !== "pen" && Math.abs(s.w) < 4 && Math.abs(s.h) < 4) {
        setShapes((list) => list.filter((x) => x.id !== s.id));
        undoStack.pop();
      }
      setSelected(null);
      persist();
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function onDblClick(e: MouseEvent) {
    const p = toLogical(e);
    const hit = hitTest(p);
    if (hit && (hit.kind === "rect" || hit.kind === "text" || hit.kind === "ellipse")) {
      openTextEditor(hit.id, e.clientX, e.clientY);
    }
  }

  function openTextEditor(id: string, clientX: number, clientY: number) {
    const r = wrapEl.getBoundingClientRect();
    setEditing({ id, sx: clientX - r.left, sy: clientY - r.top });
  }

  function commitText(id: string, value: string) {
    setEditing(null);
    const text = value.trim();
    const target = shapes.find((s) => s.id === id);
    if (!target) return;
    if (!text && target.kind === "text") {
      setShapes((list) => list.filter((s) => s.id !== id));
    } else {
      setShapes((s) => s.id === id, "text", text);
    }
    persist();
  }

  function deleteSelected() {
    const id = selected();
    if (!id) return;
    pushUndo();
    setShapes((list) => list.filter((s) => s.id !== id));
    setSelected(null);
    persist();
  }

  function clearAll() {
    if (shapes.length === 0) return;
    pushUndo();
    setShapes(reconcile([]));
    setSelected(null);
    persist();
  }

  function onKeyDown(e: KeyboardEvent) {
    const el = e.target as HTMLElement | null;
    if (el && (el.isContentEditable || ["INPUT", "TEXTAREA"].includes(el.tagName))) return;
    if (e.key === "Backspace" || e.key === "Delete") {
      e.preventDefault();
      deleteSelected();
    } else if (e.key === "z" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      undo();
    } else if (e.key === "Escape") {
      setSelected(null);
      setEditing(null);
    }
  }

  // ---- pasted and dropped images: the annotate-over-a-screenshot flow.

  /** Downscales to keep localStorage and the exported PNG reasonable. */
  async function toDataUrl(blob: Blob): Promise<string | null> {
    try {
      const bmp = await createImageBitmap(blob);
      const scale = Math.min(1, 1400 / bmp.width, 1400 / bmp.height);
      const c = document.createElement("canvas");
      c.width = Math.max(1, Math.round(bmp.width * scale));
      c.height = Math.max(1, Math.round(bmp.height * scale));
      c.getContext("2d")?.drawImage(bmp, 0, 0, c.width, c.height);
      bmp.close();
      const png = c.toDataURL("image/png");
      return png.length > 2_500_000 ? c.toDataURL("image/jpeg", 0.85) : png;
    } catch {
      return null;
    }
  }

  async function addImageBlob(blob: Blob) {
    const href = await toDataUrl(blob);
    if (!href) {
      toast("warn", "That image could not be read.");
      return;
    }
    const probe = new Image();
    await new Promise<void>((res, rej) => {
      probe.onload = () => res();
      probe.onerror = () => rej(new Error("bad image"));
      probe.src = href;
    });
    const scale = Math.min(1, (W * 0.82) / probe.width, (H * 0.82) / probe.height);
    const w = Math.round(probe.width * scale);
    const h = Math.round(probe.height * scale);
    pushUndo();
    // Images go UNDER the ink: first in the array paints first, so every
    // stroke, box and note annotates on top of it.
    setShapes(
      produce((list) => {
        list.unshift({
          id: sid(),
          kind: "image",
          color: "#8c8c8c",
          x: Math.round((W - w) / 2),
          y: Math.round((H - h) / 2),
          w,
          h,
          href,
        });
      }),
    );
    setTool("pen");
    persist();
    toast("ok", "Image added — annotate over it; drag it in select mode.");
  }

  function onPaste(e: ClipboardEvent) {
    const el = e.target as HTMLElement | null;
    if (el && ["INPUT", "TEXTAREA"].includes(el.tagName)) return;
    const item = [...(e.clipboardData?.items ?? [])].find((i) => i.type.startsWith("image/"));
    const file = item?.getAsFile();
    if (!file) return;
    e.preventDefault();
    void addImageBlob(file);
  }

  function onDrop(e: DragEvent) {
    const file = [...(e.dataTransfer?.files ?? [])].find((f) => f.type.startsWith("image/"));
    if (!file) return;
    e.preventDefault();
    void addImageBlob(file);
  }

  onMount(() => {
    wrapEl.addEventListener("keydown", onKeyDown);
    wrapEl.addEventListener("paste", onPaste);
    onCleanup(() => {
      wrapEl.removeEventListener("keydown", onKeyDown);
      wrapEl.removeEventListener("paste", onPaste);
    });
  });

  // ---- share with the agent

  async function toPngBase64(): Promise<string> {
    const clone = svgEl.cloneNode(true) as SVGSVGElement;
    clone.setAttribute("width", String(W));
    clone.setAttribute("height", String(H));
    clone.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    for (const n of clone.querySelectorAll("[data-ui]")) n.remove();
    const xml = new XMLSerializer().serializeToString(clone);
    const img = new Image();
    await new Promise<void>((res, rej) => {
      img.onload = () => res();
      img.onerror = () => rej(new Error("could not rasterise the sketch"));
      img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(xml)}`;
    });
    const canvas = document.createElement("canvas");
    canvas.width = W;
    canvas.height = H;
    const g = canvas.getContext("2d");
    if (!g) throw new Error("no canvas context");
    // Always a white ground: the exported sketch must read the same for the
    // model and the user regardless of app theme.
    g.fillStyle = "#ffffff";
    g.fillRect(0, 0, W, H);
    g.drawImage(img, 0, 0);
    return canvas.toDataURL("image/png").split(",")[1] ?? "";
  }

  /** Coordinate summary so non-vision models can still read the sketch. */
  function describeShapes(): string {
    if (shapes.length === 0) return "";
    const pct = (v: number, total: number) => `${Math.round((v / total) * 100)}%`;
    const lines = shapes.slice(0, 50).map((s) => {
      const b = bboxOf(s);
      const at = `x${pct(b.x, W)} y${pct(b.y, H)}`;
      const size = `${pct(b.w, W)}×${pct(b.h, H)}`;
      switch (s.kind) {
        case "rect":
          return `- box${s.text ? ` "${s.text}"` : ""} at ${at}, size ${size}`;
        case "ellipse":
          return `- ellipse${s.text ? ` "${s.text}"` : ""} at ${at}, size ${size}`;
        case "arrow":
          return `- arrow from (x${pct(s.x, W)} y${pct(s.y, H)}) to (x${pct(s.x + s.w, W)} y${pct(s.y + s.h, H)})`;
        case "text":
          return `- note "${s.text ?? ""}" at ${at}`;
        case "image":
          return `- pasted reference image at ${at}, size ${size} (annotations over it are the asks)`;
        default:
          return `- freehand mark around ${at}, extent ${size}`;
      }
    });
    return `Sketch layout (positions as % of a ${W}×${H} canvas):\n${lines.join("\n")}`;
  }

  /** A sketch node parked on this chat, waiting for a drawing. */
  const pendingSketch = () => {
    const cid = chatId();
    return cid ? getSession(cid).live.sketch : null;
  };

  async function sendToAgent() {
    const cid = chatId();
    if (!cid) {
      toast("warn", "Open a chat first — the sketch is sent into the conversation.");
      return;
    }
    if (shapes.length === 0) {
      toast("warn", "Draw something first.");
      return;
    }
    const session = getSession(cid);
    const parked = session.live.sketch;
    if (!parked && session.live.runId) {
      toast("warn", "A run is in progress — send the sketch when it finishes.");
      return;
    }
    setSending(true);
    try {
      if (parked && session.live.runId) {
        // A sketch node is waiting: the drawing resolves it and the graph
        // moves on with the layout description as this node's output.
        const b64 = await toPngBase64();
        const att = await api.attachBytes(cid, `sketch-${Date.now()}.png`, "image/png", b64);
        const payload = [
          note().trim() || "This sketch is how the layout should be — honour it.",
          describeShapes(),
          `(The drawing itself is saved in this chat as "${att.name}".)`,
        ].join("\n\n");
        await api.answerQuestion(session.live.runId, parked.id, payload);
        session.setLive(
          produce((l) => {
            l.sketch = null;
            l.status = "tools";
          }),
        );
        toast("ok", "Sketch handed to the running graph.");
      } else if (target()) {
        // Drawing FOR a sketch node, ahead of any run: the drawing becomes
        // that node's pinned artifact, so runs use it without pausing.
        const t = target()!;
        const content = [
          "## The user's sketch",
          "",
          note().trim() || "This sketch is how the layout should be — honour it.",
          "",
          describeShapes(),
        ].join("\n");
        const updated = await api.graphSetArtifact(t.graphId, t.nodeId, content);
        megState.upsert(updated);
        toast("ok", `Sketch saved to "${t.label}" — pinned, so runs use it without pausing.`);
      } else {
        const b64 = await toPngBase64();
        const att = await api.attachBytes(cid, `sketch-${Date.now()}.png`, "image/png", b64);
        const message = [
          note().trim() ||
            "Here is my sketch of how the wireframe/layout should change. Read the boxes, arrows and notes, and apply them to the design and the implementation.",
          describeShapes(),
        ]
          .filter(Boolean)
          .join("\n\n");
        await session.send(message, [att.id]);
        toast("ok", "Sketch sent to the agent.");
      }
      setNote("");
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setSending(false);
    }
  }

  // ---- rendering

  const arrowHead = (s: SketchShape): string => {
    const x2 = s.x + s.w;
    const y2 = s.y + s.h;
    const angle = Math.atan2(s.h, s.w);
    const len = 12;
    const a1 = angle + Math.PI * 0.85;
    const a2 = angle - Math.PI * 0.85;
    return `${x2},${y2} ${x2 + len * Math.cos(a1)},${y2 + len * Math.sin(a1)} ${x2 + len * Math.cos(a2)},${y2 + len * Math.sin(a2)}`;
  };

  const TOOLS: { id: Tool; glyph: string; title: string }[] = [
    { id: "select", glyph: "✥", title: "Select and move (drag) · double-click to label · ⌫ deletes" },
    { id: "pen", glyph: "✎", title: "Freehand pen" },
    { id: "rect", glyph: "▭", title: "Box — a region or component" },
    { id: "ellipse", glyph: "◯", title: "Ellipse" },
    { id: "arrow", glyph: "↗", title: "Arrow — point at what should change" },
    { id: "text", glyph: "T", title: "Text note — click to place" },
  ];

  return (
    <div class="sketch" ref={wrapEl} tabindex="0">
      <div class="code-head">
        <For each={TOOLS}>
          {(t) => (
            <button
              type="button"
              class="btn sketch-tool"
              data-size="sm"
              data-on={tool() === t.id}
              title={t.title}
              onClick={() => setTool(t.id)}
            >
              {t.glyph}
            </button>
          )}
        </For>
        <span class="vdivider" />
        <For each={[...PALETTE, ...extraColors()]}>
          {(c) => (
            <button
              type="button"
              class="sketch-color"
              data-on={color() === c}
              style={{ background: c }}
              title={c}
              onClick={() => setColor(c)}
            />
          )}
        </For>
        <div class="spacer" />
        <button type="button" class="btn" data-size="sm" title="Undo  ⌘Z" onClick={undo}>
          <Undo size={12} />
        </button>
        <button type="button" class="btn" data-size="sm" title="Clear the canvas" onClick={clearAll}>
          <Trash size={12} />
        </button>
      </div>

      {/* biome-ignore lint/a11y/noStaticElementInteractions: drop target for image files; paste (⌘V) is the keyboard path */}
      <div class="sketch-canvas-wrap" onDragOver={(e) => e.preventDefault()} onDrop={onDrop}>
        <svg
          ref={svgEl}
          class="sketch-canvas"
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="xMidYMid meet"
          data-tool={tool()}
          onPointerDown={onPointerDown}
          onDblClick={onDblClick}
          role="img"
          aria-label="Sketch canvas"
        >
          <For each={shapes}>
            {(s) => {
              const sel = () => selected() === s.id;
              return (
                <g class="sketch-shape" data-selected={sel()}>
                  <Show when={s.kind === "image" && s.href}>
                    <image
                      href={s.href}
                      x={s.x}
                      y={s.y}
                      width={Math.abs(s.w)}
                      height={Math.abs(s.h)}
                      preserveAspectRatio="none"
                    />
                  </Show>
                  <Show when={s.kind === "rect"}>
                    <rect
                      x={Math.min(s.x, s.x + s.w)}
                      y={Math.min(s.y, s.y + s.h)}
                      width={Math.abs(s.w)}
                      height={Math.abs(s.h)}
                      rx={4}
                      fill={`${s.color}22`}
                      stroke={s.color}
                      stroke-width={2}
                    />
                  </Show>
                  <Show when={s.kind === "ellipse"}>
                    <ellipse
                      cx={s.x + s.w / 2}
                      cy={s.y + s.h / 2}
                      rx={Math.abs(s.w / 2)}
                      ry={Math.abs(s.h / 2)}
                      fill={`${s.color}22`}
                      stroke={s.color}
                      stroke-width={2}
                    />
                  </Show>
                  <Show when={s.kind === "arrow"}>
                    <line
                      x1={s.x}
                      y1={s.y}
                      x2={s.x + s.w}
                      y2={s.y + s.h}
                      stroke={s.color}
                      stroke-width={2.5}
                    />
                    <polygon points={arrowHead(s)} fill={s.color} />
                  </Show>
                  <Show when={s.kind === "pen" && s.points?.length}>
                    <polyline
                      points={(s.points ?? []).map((p) => `${p.x},${p.y}`).join(" ")}
                      fill="none"
                      stroke={s.color}
                      stroke-width={2.5}
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </Show>
                  <Show when={(s.kind === "rect" || s.kind === "ellipse") && s.text}>
                    <text
                      x={s.x + s.w / 2}
                      y={s.y + s.h / 2}
                      text-anchor="middle"
                      dominant-baseline="middle"
                      fill={s.color}
                      font-size="17"
                      font-family="ui-sans-serif, system-ui, sans-serif"
                    >
                      {s.text}
                    </text>
                  </Show>
                  <Show when={s.kind === "text"}>
                    <text
                      x={s.x}
                      y={s.y}
                      dominant-baseline="hanging"
                      fill={s.color}
                      font-size="19"
                      font-family="ui-sans-serif, system-ui, sans-serif"
                    >
                      {s.text}
                    </text>
                  </Show>
                  <Show when={sel()}>
                    {(() => {
                      const b = bboxOf(s);
                      return (
                        <rect
                          data-ui="1"
                          x={b.x - 5}
                          y={b.y - 5}
                          width={b.w + 10}
                          height={b.h + 10}
                          fill="none"
                          stroke="var(--select, #4a9eff)"
                          stroke-width={1.5}
                          stroke-dasharray="5 4"
                        />
                      );
                    })()}
                  </Show>
                </g>
              );
            }}
          </For>
        </svg>

        <Show when={editing()}>
          {(ed) => (
            <input
              class="sketch-text-input"
              style={{ left: `${ed().sx}px`, top: `${ed().sy}px` }}
              value={shapes.find((s) => s.id === ed().id)?.text ?? ""}
              ref={(el) => setTimeout(() => el.focus(), 20)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") commitText(ed().id, e.currentTarget.value);
                if (e.key === "Escape") setEditing(null);
              }}
              onBlur={(e) => commitText(ed().id, e.currentTarget.value)}
            />
          )}
        </Show>

        <Show when={shapes.length === 0}>
          <div class="sketch-empty">
            Draw the layout you want — boxes for regions, arrows for moves, text for notes.
            <br />
            Paste (⌘V) or drop a screenshot to annotate over it, or click <b>✎ sketch</b> on any design block
            in the chat to start from its wireframe.
          </div>
        </Show>

        <Show when={pendingSketch()}>
          {(req) => (
            <div class="sketch-waiting" title={req().prompt}>
              ◈ The graph is paused on this sketch — {req().prompt}
            </div>
          )}
        </Show>

        <Show when={!pendingSketch() && target()}>
          {(t) => (
            <div class="sketch-target">
              <span class="truncate">
                ✏ Drawing for the “{t().label}” node — Save stores it as that node's output
              </span>
              <button
                type="button"
                title="Detach from the node — Send goes to the chat instead"
                onClick={() => setTarget(null)}
              >
                ×
              </button>
            </div>
          )}
        </Show>
      </div>

      <div class="sketch-send">
        <input
          class="input"
          placeholder="What should the agent do with this sketch? (optional)"
          value={note()}
          onInput={(e) => setNote(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void sendToAgent();
          }}
        />
        <button
          type="button"
          class="btn"
          data-variant="primary"
          data-size="sm"
          disabled={sending()}
          title={
            pendingSketch()
              ? "Resolve the waiting sketch node with this drawing"
              : target()
                ? "Store the drawing as the sketch node's pinned output"
                : "Attach the sketch as an image and send it into this chat"
          }
          onClick={() => void sendToAgent()}
        >
          <Check size={12} />{" "}
          {sending()
            ? "Sending…"
            : pendingSketch()
              ? "Send to waiting graph"
              : target()
                ? "Save to node"
                : "Send to agent"}
        </button>
      </div>
    </div>
  );
}
