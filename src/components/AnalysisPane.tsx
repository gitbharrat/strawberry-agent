// The architecture pane: a living node map of the workspace.
//
// Files are nodes sized by how connected they are; edges thicken with
// coupling; the ring around each node is its lint-derived quality. Click a
// file for its functions — each described by the selected model — and click a
// function to light up its lookalikes across the codebase. All rendering is
// one canvas: no DOM per node, so a thousand files pan at 60fps.

import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { Close, File as FileIcon, History, Network, Refresh, Sparkle, Stop } from "./Icons";
import { ModelPicker } from "./ModelPicker";
import { api, Channel } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { formatWhen, openPath, pickDirectory } from "~/lib/actions";
import { getSession } from "~/lib/session";
import {
  buildFnGraph,
  buildGraph,
  similarFunctions,
  stepSim,
  topDirUnder,
  type GEdge,
  type GNode,
  type Graph,
  type SimilarHit,
} from "~/lib/graph";
import type { AnalysisEvent, AnalysisSnapshot, ArchDoc, ArchFile, ArchFn } from "~/lib/types";
import { errText } from "~/lib/err";

interface ScanState {
  message: string;
  done: number;
  total: number;
}

const GROUP_HUES = [210, 155, 35, 265, 190, 320, 95, 15, 235, 130, 285, 60];

const KIND_GLYPHS: Record<string, string> = {
  entry: "▶",
  orchestrator: "◆",
  ui: "▣",
  state: "◉",
  model: "≡",
  util: "○",
  io: "⇅",
  config: "☰",
  test: "✓",
};

/** The file-type channel: each architectural kind owns a colour, worn as the
 * tile's left accent (and the dot hue) the way execution-graph nodes wear
 * their category. Folder identity lives in the territory hull instead. */
const KIND_COLORS: Record<string, { dark: string; light: string }> = {
  entry: { dark: "#e2687a", light: "#b81f3d" },
  orchestrator: { dark: "#a98cf0", light: "#6f4bc4" },
  ui: { dark: "#5f9fdc", light: "#2a5fa8" },
  state: { dark: "#4cc2ce", light: "#136b74" },
  model: { dark: "#e2a63d", light: "#8a5a08" },
  io: { dark: "#4fbf8f", light: "#16794c" },
  util: { dark: "#9aa0a8", light: "#5b6068" },
  config: { dark: "#8a8f98", light: "#6b7280" },
  test: { dark: "#7d9c6f", light: "#4e7a3e" },
};

function kindColor(kind: string | null, dark: boolean, fallback: string): string {
  const c = kind ? KIND_COLORS[kind] : undefined;
  return c ? (dark ? c.dark : c.light) : fallback;
}

/** Screen-space tile metrics. Cards ease down to half size as the camera
 * pulls back — a fully constant card towers over a distant world — and are
 * capped at their natural size when zoomed in. Drawing stays screen-space,
 * so they render crisp at every step. */
const TILE_H = 18;
const TILE_FONT_PX = 9.5;
const MONO = "ui-monospace, SFMono-Regular, Menlo, monospace";

export function AnalysisPane() {
  const chatId = () => store.activeChatId();
  const [snap, setSnap] = createSignal<AnalysisSnapshot | null>(null);
  const [scanning, setScanning] = createSignal<ScanState | null>(null);
  const [filter, setFilter] = createSignal("");
  const [selected, setSelected] = createSignal<string | null>(null);
  const [selectedFn, setSelectedFn] = createSignal<string | null>(null);
  const [similar, setSimilar] = createSignal<SimilarHit[]>([]);
  // The annotation model is the pane's own choice, remembered across sessions;
  // unset, it follows the chat like everything else.
  const [scanModel, setScanModel] = createSignal<string | null>(restoreScanModel());
  // Folder scope: a workspace-relative path prefix. The map shows only files
  // under it, re-clustered by its subfolders, and Scan annotates only those.
  const [scope, setScope] = createSignal<string | null>(null);
  // A file opened up into its functions (double-click). Back returns to the map.
  const [drill, setDrill] = createSignal<string | null>(null);
  // Legend type highlight: a kind name or "serves" — dims everything else.
  const [kindFilter, setKindFilter] = createSignal<string | null>(null);

  const doc = () => snap()?.doc ?? null;
  const model = () => scanModel() ?? store.activeChat()?.model ?? store.settings()?.defaultModel ?? "";

  /** The document the canvas and panel actually see: scope applied. */
  const viewDoc = createMemo(() => {
    const d = doc();
    const s = scope();
    if (!d || !s) return d;
    const files = d.files.filter((f) => f.id === s || f.id.startsWith(`${s}/`));
    const keep = new Set(files.map((f) => f.id));
    return {
      ...d,
      files,
      edges: d.edges.filter((e) => keep.has(e.from) && keep.has(e.to)),
      fileCount: files.length,
      fnCount: files.reduce((n, f) => n + f.functions.length, 0),
    };
  });

  /** One level up: services/frontend/src → services/frontend → services → all. */
  function scopeUp() {
    const s = scope();
    if (!s) return;
    const i = s.lastIndexOf("/");
    setScope(i > 0 ? s.slice(0, i) : null);
    selectFile(null);
  }

  function enterScope(dir: string) {
    setScope(scope() === dir ? null : dir);
    selectFile(null);
  }

  function enterDrill(fileId: string) {
    setDrill(fileId);
    selectFile(fileId);
  }

  /** Back from a drill: keep the file selected, drop any function selection
   * and its similarity highlight — otherwise the whole map stays dimmed to
   * the lookalikes of a function you are no longer looking at. */
  function exitDrill() {
    const file = drill();
    setDrill(null);
    if (file) selectFile(file);
  }

  /** Double-click in the drill view: jump the editor to that function. */
  function openFn(name: string) {
    const d = doc();
    const fileId = drill();
    if (!d || !fileId) return;
    const fn = d.files.find((f) => f.id === fileId)?.functions.find((x) => x.name === name);
    void openPath(`${d.root}/${fileId}`, fn?.line);
  }

  function pickModel(id: string) {
    setScanModel(id);
    try {
      localStorage.setItem("sa.archmodel", id);
    } catch {
      /* private mode */
    }
  }

  async function load() {
    const id = chatId();
    if (!id) return;
    try {
      setSnap(await api.analysisGet(id));
    } catch (e) {
      toast("error", errText(e));
    }
  }

  onMount(() => void load());
  // The cwd is a dependency too: connecting a remote workspace (or setting a
  // folder) must swap the pane from "no folder" to the map without a chat
  // switch in between.
  createEffect(
    on(
      () => [chatId(), store.activeChat()?.cwd],
      () => {
        setScope(null);
        setDrill(null);
        setKindFilter(null);
        void load();
      },
      { defer: true },
    ),
  );

  // The map refreshes itself in the backend after each agent run; pick the
  // result up as soon as the run settles.
  createEffect(
    on(
      () => {
        const id = chatId();
        return id ? getSession(id).live.status : "idle";
      },
      (status, prev) => {
        if (status === "idle" && prev && prev !== "idle" && !scanning()) {
          // The auto-update runs in the background; give it a beat to land.
          setTimeout(() => void load(), 1500);
        }
      },
      { defer: true },
    ),
  );

  async function scan() {
    const id = chatId();
    if (!id || scanning()) return;
    setScanning({ message: "Starting…", done: 0, total: 0 });
    const ch = new Channel<AnalysisEvent>();
    ch.onmessage = (ev) => {
      if (ev.type === "phase") setScanning((s) => ({ ...(s ?? { done: 0, total: 0 }), message: ev.message }));
      if (ev.type === "progress") setScanning((s) => (s ? { ...s, done: ev.done, total: ev.total } : s));
      if (ev.type === "notice") toast("warn", ev.text);
    };
    try {
      // The AI pass is not optional: the map's meaning comes from the model.
      // Without a key the backend degrades to the static skeleton and says so.
      const fresh = await api.analysisScan(id, true, scanModel(), scope(), ch);
      setSnap((s) => ({ root: s?.root ?? fresh.root, doc: fresh }));
      toast("ok", `Mapped ${fresh.fileCount} files, ${fresh.fnCount} functions`);
    } catch (e) {
      const msg = errText(e);
      if (!msg.includes("cancelled")) toast("error", msg);
    } finally {
      setScanning(null);
    }
  }

  async function cancelScan() {
    const id = chatId();
    if (id) await api.analysisCancel(id).catch(() => {});
  }

  async function chooseFolder() {
    const id = chatId();
    if (!id) return;
    const dir = await pickDirectory("Working folder for this chat");
    if (!dir) return;
    await api.updateChat({ id, cwd: dir });
    void store.refreshChats();
    void load();
  }

  function selectFile(fileId: string | null) {
    setSelected(fileId);
    setSelectedFn(null);
    setSimilar([]);
  }

  function selectFn(fileId: string, fnName: string) {
    const d = doc();
    if (!d) return;
    setSelected(fileId);
    setSelectedFn(fnName);
    setSimilar(similarFunctions(d, fileId, fnName));
  }

  /** Kind → colour rows for the footer key, theme-aware. */
  const kindLegend = createMemo(() => {
    void store.settings();
    const dark = document.documentElement.dataset.theme !== "light";
    return Object.entries(KIND_COLORS).map(([name, c]) => ({ name, color: dark ? c.dark : c.light }));
  });

  const selectedFile = createMemo(() => {
    const d = doc();
    const id = selected();
    return d && id ? (d.files.find((f) => f.id === id) ?? null) : null;
  });

  return (
    <div class="arch">
      <div class="code-head">
        <Show when={drill() || scope()}>
          <button
            type="button"
            class="btn"
            data-size="sm"
            title={drill() ? "Back to the map" : "Up one folder"}
            onClick={() => (drill() ? exitDrill() : scopeUp())}
          >
            ‹ Back
          </button>
        </Show>
        <Show when={drill()}>
          <span class="chip" data-tone="accent" title="Functions of this file">
            {drill()}
          </span>
        </Show>
        <Show when={!drill() && scope()}>
          <button
            type="button"
            class="chip"
            data-tone="accent"
            title="Clear folder filter"
            onClick={() => enterScope(scope()!)}
          >
            {scope()} ✕
          </button>
        </Show>
        <Show when={viewDoc()}>
          {(d) => (
            <>
              <span class="chip">{d().fileCount} files</span>
              <span class="chip">{d().fnCount} fns</span>
              <Show when={d().model}>
                <span class="chip" data-tone="accent" title="Model that wrote the descriptions">
                  {shortModel(d().model ?? "")}
                </span>
              </Show>
            </>
          )}
        </Show>
        <div class="spacer" />
        <Show when={doc()}>
          <input
            class="input arch-filter"
            data-mono="true"
            placeholder="filter…"
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
        </Show>
        <Show when={snap()?.root}>
          <ModelPicker value={model()} onChange={pickModel} drop="down" />
          <Show
            when={!scanning()}
            fallback={
              <button
                type="button"
                class="btn"
                data-size="sm"
                data-variant="danger"
                onClick={() => void cancelScan()}
              >
                <Stop size={11} /> Stop
              </button>
            }
          >
            <button
              type="button"
              class="btn"
              data-size="sm"
              title={`Scan ${scope() ?? "the workspace"} — ${model()} describes anything new or changed`}
              onClick={() => void scan()}
            >
              <Refresh size={12} /> Scan
            </button>
          </Show>
        </Show>
      </div>

      <Show when={scanning()}>
        {(s) => (
          <div class="arch-progress" title={s().message}>
            <div
              class="arch-progress-fill"
              data-indeterminate={s().total === 0}
              style={{ width: s().total > 0 ? `${Math.round((s().done / s().total) * 100)}%` : "40%" }}
            />
            <span class="arch-progress-msg truncate">
              {s().message}
              {s().total > 0 ? ` (${s().done}/${s().total})` : ""}
            </span>
          </div>
        )}
      </Show>

      <Show
        when={snap()}
        fallback={
          <div class="empty">
            <p>Loading…</p>
          </div>
        }
      >
        <Show
          when={snap()?.root}
          fallback={
            <div class="empty">
              <Network size={28} />
              <h2>No working folder</h2>
              <p>The architecture map is built from this chat's workspace. Pick the folder first.</p>
              <button type="button" class="btn" data-variant="primary" onClick={() => void chooseFolder()}>
                Set folder
              </button>
            </div>
          }
        >
          <Show
            when={doc()}
            fallback={
              <div class="empty">
                <Network size={28} />
                <h2>Map this codebase</h2>
                <p>
                  Builds a dependency graph of every file and function — connections, quality lints, and
                  one-line descriptions written by <strong>{shortModel(model())}</strong>. Saved to{" "}
                  <code>.strawberry/architecture.md</code>, handed to the agent in every chat, and refreshed
                  after each change.
                </p>
                <button
                  type="button"
                  class="btn"
                  data-variant="primary"
                  disabled={!!scanning()}
                  onClick={() => void scan()}
                >
                  <Sparkle size={13} /> Analyze codebase
                </button>
                <p class="muted" style={{ "font-size": "var(--fs-xs)" }}>
                  Later scans only re-describe what changed, so tokens stay cheap.
                </p>
              </div>
            }
          >
            {/* Deliberately non-keyed: a re-scan swaps the doc in place and the
                canvas keeps its camera and layout instead of remounting. */}
            <div class="arch-body">
              <GraphCanvas
                doc={viewDoc()!}
                drillFile={drill()}
                groupBase={scope()}
                kindFilter={drill() ? null : kindFilter()}
                filter={filter().trim().toLowerCase()}
                selected={drill() ? selectedFn() : selected()}
                similar={similar()}
                onSelect={(id) => {
                  const file = drill();
                  if (file) {
                    if (id) selectFn(file, id);
                    else {
                      setSelectedFn(null);
                      setSimilar([]);
                    }
                  } else selectFile(id);
                }}
                onActivate={(id) => (drill() ? openFn(id) : enterDrill(id))}
                onGroup={enterScope}
              />
              <ArchPanel
                doc={viewDoc()!}
                file={selectedFile()}
                selectedFn={selectedFn()}
                similar={similar()}
                filter={filter().trim().toLowerCase()}
                onSelectFile={selectFile}
                onSelectFn={selectFn}
                onClose={() => selectFile(null)}
              />
            </div>
          </Show>
        </Show>
      </Show>

      <Show when={doc()}>
        <div class="arch-legend">
          <span>{drill() ? "color = tag" : "island = folder · color = type"}</span>
          <span>size = connections</span>
          <Show when={!drill()}>
            <For each={kindLegend()}>
              {(k) => (
                <button
                  type="button"
                  class="arch-legend-key"
                  data-on={kindFilter() === k.name}
                  style={{ color: k.color }}
                  title={`Highlight ${k.name} files`}
                  onClick={() => setKindFilter(kindFilter() === k.name ? null : k.name)}
                >
                  ● {k.name}
                </button>
              )}
            </For>
            <button
              type="button"
              class="arch-legend-key"
              data-on={kindFilter() === "serves"}
              style={{ color: "var(--leaf-400)" }}
              title="Highlight files exposing API routes or pub/sub topics"
              onClick={() => setKindFilter(kindFilter() === "serves" ? null : "serves")}
            >
              ● serves api/event
            </button>
          </Show>
          <span style={{ color: "var(--warn)" }}>border = lint flag</span>
          <span style={{ color: "var(--leaf-400)" }}>⌁ route/event</span>
          <span class="arch-legend-out">→ uses</span>
          <span class="arch-legend-in">→ used by</span>
          <span class="arch-legend-sim">◌ similar</span>
          <span class="muted">double-click: open</span>
          <div class="spacer" />
          <span class="tabular">{formatWhen(doc()?.generatedAt ?? 0)}</span>
        </div>
      </Show>
    </div>
  );
}

function shortModel(id: string): string {
  const tail = id.split("/").pop() ?? id;
  return tail.length > 24 ? `${tail.slice(0, 22)}…` : tail;
}

function maxCxOf(f: ArchFile): number {
  return f.functions.reduce((m, fn) => Math.max(m, fn.complexity), 0);
}

/** Complexity heat bucket shared by chips and bars. */
function cxHeat(cx: number): "hot" | "warm" | "cool" {
  return cx >= 16 ? "hot" : cx >= 8 ? "warm" : "cool";
}

/** Severity tint for a big-O string: quadratic and worse burn red,
 * linearithmic and multi-variable warm, linear and below stay quiet. */
function oHeat(o: string): "hot" | "warm" | "cool" {
  const s = o.toLowerCase().replace(/\s+/g, "");
  if (/²|³|\^2|\^3|n2\)|2\^|n!|exp/.test(s)) return "hot";
  if (/nlog|n\*m|nm\)|n×m|mn\)/.test(s)) return "warm";
  return "cool";
}

function sortFns(fns: ArchFn[], mode: "line" | "cx"): ArchFn[] {
  return mode === "cx" ? [...fns].sort((a, b) => b.complexity - a.complexity) : fns;
}

function restoreScanModel(): string | null {
  try {
    return localStorage.getItem("sa.archmodel");
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------- the canvas

interface CanvasProps {
  doc: ArchDoc;
  /** File opened into its functions; null = the file map. */
  drillFile: string | null;
  /** Folder scope — clustering follows its subfolders. */
  groupBase: string | null;
  filter: string;
  /** Legend type highlight: a kind name or "serves"; null = off. */
  kindFilter: string | null;
  selected: string | null;
  similar: SimilarHit[];
  onSelect: (id: string | null) => void;
  /** Double-click: drill into a file, or open a function in the editor. */
  onActivate: (id: string) => void;
  /** Legend click: scope the map to one directory. */
  onGroup: (dir: string) => void;
}

function GraphCanvas(props: CanvasProps) {
  let wrap!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  let graph: Graph = { nodes: [], edges: [], groups: [], anchors: [], byId: new Map(), droppedEdges: 0 };
  const camera = { k: 1, tx: 0, ty: 0 };
  let width = 0;
  let height = 0;
  let alpha = 0;
  let rafId = 0;
  let disposed = false;
  let fitted = false;
  // After a rebuild the layout drifts while it settles; one re-fit at the end
  // frames the finished shape — unless the user has already taken the camera.
  let needsRefit = false;
  let userMoved = false;
  // Trackpad momentum keeps firing wheel events after a double-click; for a
  // beat after a rebuild those must not steal the camera from auto-fit.
  let rebuiltAt = 0;
  function takeCamera() {
    if (performance.now() - rebuiltAt > 350) userMoved = true;
  }
  /** Size factor: cards shrink toward half size as the world recedes. */
  function sf(): number {
    return Math.min(Math.max(camera.k, 0.5), 1.05);
  }
  function th(): number {
    return TILE_H * sf();
  }
  // Edge width-buckets survive between frames — membership only changes with
  // the graph or the selection, never with node positions.
  let edgeBuckets: Map<number, GEdge[]> | null = null;
  let bucketSel: string | null = null;
  let dprUsed = 1;

  const [hover, setHover] = createSignal<string | null>(null);
  let tipEl: HTMLDivElement | undefined;
  const [dropped, setDropped] = createSignal(0);
  // Mirrors camera.k across the tile-collapse threshold so the legend can
  // explain why the names went away.
  const [zoomedOut, setZoomedOut] = createSignal(false);
  function syncZoomHint() {
    setZoomedOut(camera.k < 0.55);
  }

  const similarByFile = createMemo(() => {
    const m = new Map<string, number>();
    for (const s of props.similar) {
      m.set(s.fileId, Math.max(m.get(s.fileId) ?? 0, s.score));
    }
    return m;
  });

  const neighborSet = createMemo(() => {
    const set = new Set<string>();
    const sel = props.selected;
    if (!sel) return set;
    for (const e of props.doc.edges) {
      if (e.from === sel) set.add(e.to);
      if (e.to === sel) set.add(e.from);
    }
    return set;
  });

  /** The selection's directed edges — what it uses vs what uses it. This is
   * the execution-flow view: accent arrows point at dependencies, steel
   * arrows come in from dependents. */
  const selFlow = createMemo(() => {
    const sel = props.selected;
    const out: { to: string; weight: number }[] = [];
    const inc: { from: string; weight: number }[] = [];
    if (sel) {
      for (const e of props.doc.edges) {
        if (e.from === sel) out.push({ to: e.to, weight: e.weight });
        else if (e.to === sel) inc.push({ from: e.from, weight: e.weight });
      }
    }
    return { out, inc };
  });

  function palette() {
    const cs = getComputedStyle(document.documentElement);
    const v = (name: string) => cs.getPropertyValue(name).trim();
    const dark = document.documentElement.dataset.theme !== "light";
    return {
      bg: v("--bg-sunken") || "#080808",
      text: v("--text"),
      text2: v("--text-2"),
      text3: v("--text-3"),
      accent: v("--accent"),
      select: v("--select") || "#3f6d9e",
      violet: v("--violet-400") || "#9585d6",
      leaf: v("--leaf-400") || "#3fd082",
      ok: v("--ok"),
      warn: v("--warn"),
      danger: v("--danger"),
      // Fusion-style cards: the same neutral surface and hairline the
      // execution-graph nodes use, so both graphs read as one family.
      card: v("--surface-2") || (dark ? "#191919" : "#e9e9e9"),
      borderStrong: v("--border-strong") || (dark ? "#333333" : "#c2c2c2"),
      tileText: v("--text") || (dark ? "#e6e6e6" : "#1a1a1a"),
      dark,
    };
  }

  function groupColor(g: number, dark: boolean): string {
    const hue = GROUP_HUES[g % GROUP_HUES.length] ?? 210;
    return dark ? `hsl(${hue} 40% 60%)` : `hsl(${hue} 45% 42%)`;
  }

  function qualityColor(q: number, p: ReturnType<typeof palette>): string {
    if (q >= 80) return p.ok;
    if (q >= 60) return p.warn;
    return p.danger;
  }

  function resize() {
    const rect = wrap.getBoundingClientRect();
    width = Math.max(10, rect.width);
    height = Math.max(10, rect.height);
    dprUsed = window.devicePixelRatio || 1;
    canvas.width = Math.round(width * dprUsed);
    canvas.height = Math.round(height * dprUsed);
    // No CSS size here: `inset: 0` keeps the element exactly the wrap's box.
    // Writing measured pixels back into style.width compounds with the app's
    // UI zoom and skews every click by the zoom factor.
    // The detail panel opening (a drill always opens it) shrinks this canvas
    // AFTER the drill fitted — while auto-fit is armed, re-frame for the new
    // box instead of leaving the content parked at the right edge.
    if (needsRefit && !userMoved) fitView();
    draw();
  }

  /** Event position → logical canvas coordinates, via the canvas's actual
   * on-screen box. Immune to UI zoom, fractional DPR, and any other scaling
   * between layout and visual pixels. */
  function toLocal(clientX: number, clientY: number): { x: number; y: number } {
    const rect = canvas.getBoundingClientRect();
    const sx = ((clientX - rect.left) / Math.max(1, rect.width)) * (canvas.width / dprUsed);
    const sy = ((clientY - rect.top) / Math.max(1, rect.height)) * (canvas.height / dprUsed);
    return { x: sx, y: sy };
  }

  function fitView() {
    if (graph.nodes.length === 0) return;
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of graph.nodes) {
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x);
      maxY = Math.max(maxY, n.y);
    }
    // The physics is speed-clamped and cannot diverge, but a camera aimed at
    // non-finite coordinates would render nothing at all — never allow it.
    if (!Number.isFinite(minX + minY + maxX + maxY)) {
      camera.k = 1;
      camera.tx = 0;
      camera.ty = 0;
      return;
    }
    const pad = 90;
    const w = maxX - minX + pad * 2;
    const h = maxY - minY + pad * 2;
    // Never fit past ~100%: a drilled file with three functions should show
    // them at natural size in the middle, not three giants filling the pane.
    camera.k = Math.min(1.05, Math.max(0.06, Math.min(width / w, height / h)));
    camera.tx = width / 2 - ((minX + maxX) / 2) * camera.k;
    camera.ty = height / 2 - ((minY + maxY) / 2) * camera.k;
    syncZoomHint();
  }

  function rebuild(fresh: ArchDoc) {
    const old = new Map(graph.nodes.map((n) => [n.id, n]));
    graph = props.drillFile
      ? buildFnGraph(fresh, props.drillFile, width || 800, height || 600)
      : buildGraph(fresh, width || 800, height || 600, (id) => topDirUnder(id, props.groupBase));
    edgeBuckets = null;
    setDropped(graph.droppedEdges);
    let carried = 0;
    for (const n of graph.nodes) {
      const prev = old.get(n.id);
      if (prev) {
        n.x = prev.x;
        n.y = prev.y;
        n.pinned = prev.pinned;
        carried++;
      }
    }
    // A refreshed map keeps its shape and just eases the newcomers in; a
    // brand-new one (or a scope/drill change that swaps the node set) gets
    // the full settle and a camera re-fit.
    const shapeChanged = carried !== graph.nodes.length || old.size !== graph.nodes.length;
    alpha = carried > graph.nodes.length * 0.5 && !shapeChanged ? 0.3 : 1;
    if (!fitted || shapeChanged) {
      needsRefit = true;
      userMoved = false;
      rebuiltAt = performance.now();
      // The detail panel opens in the same beat as a drill and narrows this
      // canvas AFTER we fit. Re-measure once the new layout is real — without
      // depending on ResizeObserver timing — and frame the content in the
      // box it will actually occupy.
      requestAnimationFrame(() => {
        if (disposed || userMoved) return;
        const rect = wrap.getBoundingClientRect();
        width = Math.max(10, rect.width);
        height = Math.max(10, rect.height);
        const dpr = window.devicePixelRatio || 1;
        canvas.width = Math.round(width * dpr);
        canvas.height = Math.round(height * dpr);
        dprUsed = dpr;
        fitView();
        draw();
      });
      // Pre-settle inside a hard time budget so a huge graph can never freeze
      // the pane; whatever is left settles on screen as animation.
      const t0 = performance.now();
      let i = 0;
      while (performance.now() - t0 < 180 && i < 300) {
        stepSim(graph, alpha, width || 800, height || 600);
        alpha *= 0.99;
        i++;
      }
      fitView();
      fitted = true;
    }
    loop();
  }

  function loop() {
    cancelAnimationFrame(rafId);
    const tick = () => {
      if (disposed) return;
      if (alpha > 0.02) {
        // As many ticks as fit in a few milliseconds: small graphs settle
        // almost instantly, big ones stay at frame rate and just take longer.
        const t0 = performance.now();
        do {
          stepSim(graph, alpha, width, height);
        } while (performance.now() - t0 < 6);
        alpha *= 0.96;
        // The camera FOLLOWS the expanding layout the whole way — a drill or
        // scope always lands height/width-fitted, never mid-growth. One pan
        // or zoom from the user and the follow stops for good.
        if (needsRefit && !userMoved) fitView();
        draw();
        rafId = requestAnimationFrame(tick);
      } else {
        // Auto-fit stays armed after settle: a later panel open or pane
        // resize re-frames the map until the user takes the camera.
        if (needsRefit && !userMoved) fitView();
        alpha = 0;
        draw();
      }
    };
    rafId = requestAnimationFrame(tick);
  }

  /** Everything one frame needs, computed once per draw. */
  interface Frame {
    p: ReturnType<typeof palette>;
    sel: string | null;
    sim: Map<string, number>;
    neighbors: Set<string>;
    filter: string;
    /** Type highlight from the legend: a kind name, or "serves". */
    kindFilter: string | null;
    hovId: string | null;
    /** Label rectangles already placed this frame, for collision skipping. */
    placed: { x: number; y: number; w: number; h: number }[];
  }

  function nodeAlpha(f: Frame, n: GNode): number {
    const id = n.id;
    if (f.kindFilter) {
      const miss = f.kindFilter === "serves" ? n.serves.length === 0 : n.kind !== f.kindFilter;
      if (miss) return 0.1;
    }
    if (f.filter && !id.toLowerCase().includes(f.filter)) return 0.13;
    if (f.sim.size > 0) return f.sim.has(id) || id === f.sel ? 1 : 0.3;
    if (f.sel) return id === f.sel || f.neighbors.has(id) ? 1 : 0.3;
    return 1;
  }

  /** World → screen (logical px). Everything draws in screen space so text
   * and hairlines land on the pixel grid — the difference between a blurry
   * scaled bitmap and a crisp UI card. */
  function SX(x: number): number {
    return x * camera.k + camera.tx;
  }
  function SY(y: number): number {
    return y * camera.k + camera.ty;
  }

  function draw() {
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const frame: Frame = {
      p: palette(),
      sel: props.selected,
      sim: similarByFile(),
      neighbors: neighborSet(),
      filter: props.filter,
      kindFilter: props.kindFilter,
      hovId: hover(),
      placed: [],
    };

    drawGroupHulls(ctx, frame);
    drawEdges(ctx, frame);
    // Dots first, tiles on top: a named tile is opaque and always wins.
    for (const n of graph.nodes) if (!isTile(frame, n)) drawDot(ctx, frame, n);
    for (const n of graph.nodes) if (isTile(frame, n)) drawTile(ctx, frame, n);
    ctx.globalAlpha = 1;
    positionTip();
  }

  function edgePath(ctx: CanvasRenderingContext2D, a: GNode, b: GNode) {
    const ax = SX(a.x);
    const ay = SY(a.y);
    const bx = SX(b.x);
    const by = SY(b.y);
    const dx = bx - ax;
    const dy = by - ay;
    ctx.moveTo(ax, ay);
    // A slight perpendicular bow keeps parallel edges distinguishable.
    ctx.quadraticCurveTo((ax + bx) / 2 - dy * 0.08, (ay + by) / 2 + dx * 0.08, bx, by);
  }

  /** Background edges bucketed by width — membership only changes with the
   * graph or the selection, so the buckets persist between frames and
   * thousands of edges become ~8 draw calls. */
  function edgeBucketsFor(sel: string | null): Map<number, GEdge[]> {
    if (edgeBuckets && bucketSel === sel) return edgeBuckets;
    edgeBuckets = new Map();
    bucketSel = sel;
    for (const e of graph.edges) {
      const a = graph.nodes[e.a]!;
      const b = graph.nodes[e.b]!;
      if (sel !== null && (a.id === sel || b.id === sel)) continue;
      const key = Math.round(e.width * 2);
      const list = edgeBuckets.get(key);
      if (list) list.push(e);
      else edgeBuckets.set(key, [e]);
    }
    return edgeBuckets;
  }

  function drawEdges(ctx: CanvasRenderingContext2D, f: Frame) {
    strokeBuckets(ctx, f, false);
    strokeBuckets(ctx, f, true);
    drawFlow(ctx, f);
  }

  /** One batched stroke pass. Import edges are quiet grey; channel links —
   * a route one side binds and the other calls, or a pub/sub topic — draw
   * dashed in leaf green so service seams stand out. */
  function strokeBuckets(ctx: CanvasRenderingContext2D, f: Frame, channels: boolean) {
    const focusMode = f.sel !== null || f.sim.size > 0 || f.filter !== "";
    const base = graph.edges.length > 1500 ? 0.1 : 0.2;
    ctx.lineCap = "round";
    if (channels) {
      ctx.globalAlpha = focusMode ? 0.06 : Math.min(0.5, base + 0.18);
      ctx.strokeStyle = f.p.leaf;
      ctx.setLineDash([6, 4]);
    } else {
      ctx.globalAlpha = focusMode ? 0.04 : base;
      ctx.strokeStyle = f.p.text3;
    }
    for (const [key, list] of edgeBucketsFor(f.sel)) {
      ctx.lineWidth = Math.max(0.4, Math.min(3.5, (key / 2) * camera.k));
      ctx.beginPath();
      for (const e of list) {
        if (e.via.length > 0 === channels) edgePath(ctx, graph.nodes[e.a]!, graph.nodes[e.b]!);
      }
      ctx.stroke();
    }
    ctx.setLineDash([]);
  }

  /** Execution flow for the selection: what it pulls in (accent, arrow at the
   * dependency) and what pulls it in (steel, arrow at the selection). */
  function drawFlow(ctx: CanvasRenderingContext2D, f: Frame) {
    const flow = selFlow();
    const selNode = f.sel !== null ? graph.nodes[graph.byId.get(f.sel) ?? -1] : undefined;
    if (!selNode) return;
    for (const o of flow.out) {
      const target = graph.nodes[graph.byId.get(o.to) ?? -1];
      if (target) drawFlowEdge(ctx, selNode, target, o.weight, f.p.accent);
    }
    for (const i of flow.inc) {
      const source = graph.nodes[graph.byId.get(i.from) ?? -1];
      if (source) drawFlowEdge(ctx, source, selNode, i.weight, f.p.select);
    }
  }

  /** A directed edge: curve plus an arrowhead sitting just outside the target
   * node, pointing along the curve's tangent. */
  function drawFlowEdge(
    ctx: CanvasRenderingContext2D,
    from: GNode,
    to: GNode,
    weight: number,
    color: string,
  ) {
    ctx.globalAlpha = 0.9;
    ctx.strokeStyle = color;
    ctx.lineWidth = Math.min(3, 1 + Math.log2(1 + weight) * 0.5);
    ctx.beginPath();
    edgePath(ctx, from, to);
    ctx.stroke();

    // Quadratic point + tangent at the arrow's position, in screen space.
    const fx = SX(from.x);
    const fy = SY(from.y);
    const txx = SX(to.x);
    const tyy = SY(to.y);
    const dx = txx - fx;
    const dy = tyy - fy;
    const cpx = (fx + txx) / 2 - dy * 0.08;
    const cpy = (fy + tyy) / 2 + dx * 0.08;
    const dist = Math.max(1, Math.hypot(dx, dy));
    const clearance = (to.labelW > 0 ? th() / 2 + 6 : to.r * 0.7 * camera.k) + 7;
    const t = Math.max(0.5, 1 - clearance / dist);
    const mt = 1 - t;
    const px = mt * mt * fx + 2 * mt * t * cpx + t * t * txx;
    const py = mt * mt * fy + 2 * mt * t * cpy + t * t * tyy;
    const tx = 2 * mt * (cpx - fx) + 2 * t * (txx - cpx);
    const ty = 2 * mt * (cpy - fy) + 2 * t * (tyy - cpy);
    const len = Math.max(1, Math.hypot(tx, ty));
    const ux = tx / len;
    const uy = ty / len;
    const s = 6 * sf();
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.moveTo(px, py);
    ctx.lineTo(px - ux * s - uy * s * 0.55, py - uy * s + ux * s * 0.55);
    ctx.lineTo(px - ux * s + uy * s * 0.55, py - uy * s - ux * s * 0.55);
    ctx.closePath();
    ctx.fill();
  }

  /** Convex hull, Andrew's monotone chain. Screen-space points in, ordered
   * polygon out. */
  function convexHull(pts: { x: number; y: number }[]): { x: number; y: number }[] {
    if (pts.length < 3) return pts;
    const p = [...pts].sort((a, b) => a.x - b.x || a.y - b.y);
    const cross = (o: { x: number; y: number }, a: { x: number; y: number }, b: { x: number; y: number }) =>
      (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    const lower: { x: number; y: number }[] = [];
    for (const pt of p) {
      while (lower.length >= 2 && cross(lower[lower.length - 2]!, lower[lower.length - 1]!, pt) <= 0)
        lower.pop();
      lower.push(pt);
    }
    const upper: { x: number; y: number }[] = [];
    for (let i = p.length - 1; i >= 0; i--) {
      const pt = p[i]!;
      while (upper.length >= 2 && cross(upper[upper.length - 2]!, upper[upper.length - 1]!, pt) <= 0)
        upper.pop();
      upper.push(pt);
    }
    lower.pop();
    upper.pop();
    return lower.concat(upper);
  }

  /** Territories: a soft blob hugging each cluster's true shape — convex
   * hull, inflated by a fat round-joined stroke — with the folder's name in
   * the padding band above its highest point. Blobs follow the dots, so an
   * elongated cluster gets an elongated territory instead of an acre of
   * empty box overlapping its neighbours. */
  function drawGroupHulls(ctx: CanvasRenderingContext2D, f: Frame) {
    if (graph.groups.length < 2) return;
    const byGroup: { x: number; y: number }[][] = graph.groups.map(() => []);
    for (const n of graph.nodes) {
      byGroup[n.group]!.push({ x: SX(n.x), y: SY(n.y) });
    }
    // WORLD-space padding: the blob hugs its dots at every zoom instead of
    // swallowing the gaps when zoomed out (screen-space pad did exactly that,
    // and seven stacked translucent blobs read as a grey haze).
    const pad = Math.max(8, 34 * camera.k);
    const fillA = (f.p.dark ? 0.06 : 0.08) * (graph.groups.length > 6 ? 0.75 : 1);
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = `600 11px ${MONO}`;
    const placedLabels: { x: number; y: number; w: number; h: number }[] = [];
    for (let g = 0; g < graph.groups.length; g++) {
      const pts = byGroup[g]!;
      if (pts.length < 2) continue;
      const hull = convexHull(pts);
      if (hull.length === 0) continue;

      const color = groupColor(g, f.p.dark);
      ctx.beginPath();
      ctx.moveTo(hull[0]!.x, hull[0]!.y);
      for (let i = 1; i < hull.length; i++) ctx.lineTo(hull[i]!.x, hull[i]!.y);
      ctx.closePath();
      // Fill + fat stroke of the same paint = a padded blob with round corners.
      ctx.globalAlpha = fillA;
      ctx.fillStyle = color;
      ctx.strokeStyle = color;
      ctx.lineWidth = pad * 2;
      ctx.stroke();
      ctx.fill();

      drawHullLabel(ctx, f, hull, graph.groups[g] ?? "", color, pad, placedLabels);
    }
    ctx.globalAlpha = 1;
  }

  /** The territory's name above its highest point — dodging labels already
   * placed (crowded maps stack their blobs' tops): first the top band, then
   * below the blob, else dropped for this frame. */
  function drawHullLabel(
    ctx: CanvasRenderingContext2D,
    f: Frame,
    hull: { x: number; y: number }[],
    name: string,
    color: string,
    pad: number,
    placed: { x: number; y: number; w: number; h: number }[],
  ) {
    let topX = hull[0]!.x;
    let topY = hull[0]!.y;
    let botY = hull[0]!.y;
    let cx = 0;
    for (const pt of hull) {
      cx += pt.x;
      if (pt.y < topY) {
        topY = pt.y;
        topX = pt.x;
      }
      if (pt.y > botY) botY = pt.y;
    }
    cx /= hull.length;
    let label = name;
    while (label.length > 4 && ctx.measureText(label).width > 220) {
      label = `${label.slice(0, -2)}…`;
    }
    const lw = ctx.measureText(label).width;
    const lx = Math.round((cx + topX) / 2);
    const collides = (r: { x: number; y: number; w: number; h: number }) =>
      placed.some((p) => r.x < p.x + p.w && p.x < r.x + r.w && r.y < p.y + p.h && p.y < r.y + r.h);
    const rectAt = (ly: number) => ({ x: lx - lw / 2 - 5, y: ly - 9, w: lw + 10, h: 18 });
    let ly = Math.round(topY - pad - 3);
    let rect = rectAt(ly);
    if (collides(rect)) {
      ly = Math.round(botY + pad + 10);
      rect = rectAt(ly);
      if (collides(rect)) return;
    }
    placed.push(rect);
    ctx.globalAlpha = 0.92;
    ctx.fillStyle = f.p.bg;
    ctx.fillRect(rect.x, rect.y, rect.w, rect.h);
    ctx.fillStyle = color;
    ctx.fillText(label, lx, ly + 0.5);
  }

  /** Tile height in WORLD units — the constant screen card mapped back for
   * hit-testing and anchoring. */
  function tileH(_n: GNode): number {
    return th() / camera.k;
  }

  /** Dot radius in screen px: eases down as the world recedes, floors at a
   * visible point. */
  function dotR(n: GNode): number {
    return Math.max(2, 0.55 * n.r * Math.min(Math.max(camera.k, 0.45), 1.05));
  }

  /** Does this node get a Nuke-style named tile this frame? Interaction
   * states always do; otherwise the zoom-scaled budget decides — it reaches
   * zero when zoomed far out, so the whole map collapses to shaped dots
   * under the directory landmarks. */
  function isTile(f: Frame, n: GNode): boolean {
    // A drilled file's functions are the content — always cards, never dots.
    if (props.drillFile) return true;
    if (n.id === f.sel || n.id === f.hovId || f.sim.has(n.id)) return true;
    if (camera.k < 0.55) return false;
    if (f.sel !== null && f.neighbors.has(n.id)) return true;
    const budget = Math.max(0, (camera.k - 0.5) * 150);
    return n.labelRank < budget;
  }

  function rrect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
    ctx.beginPath();
    // Safari gained roundRect in 16; the arc fallback covers anything older.
    if (typeof (ctx as { roundRect?: unknown }).roundRect === "function") {
      ctx.roundRect(x, y, w, h, r);
      return;
    }
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
  }

  /** Quiet dot for files below the label budget. One hue per island — the
   * cluster colour — so the overview reads as calm territories, not
   * confetti. Types surface on the tiles' accent bars and through the
   * legend's type highlight. */
  function drawDot(ctx: CanvasRenderingContext2D, f: Frame, n: GNode) {
    n.labelW = 0;
    ctx.globalAlpha = nodeAlpha(f, n) * (n.kind === "test" ? 0.7 : 0.92);
    const r = dotR(n);
    ctx.beginPath();
    ctx.arc(SX(n.x), SY(n.y), r, 0, Math.PI * 2);
    ctx.fillStyle =
      f.kindFilter && nodeAlpha(f, n) > 0.5
        ? kindColor(n.kind, f.p.dark, groupColor(n.group, f.p.dark))
        : groupColor(n.group, f.p.dark);
    ctx.fill();
    if (n.quality < 80) {
      ctx.lineWidth = Math.max(0.8, 1.2 * sf());
      ctx.strokeStyle = qualityColor(n.quality, f.p);
      ctx.stroke();
    }
  }

  /** A tile is one calm card: rounded rect, flat cluster fill, the name —
   * nothing else unless the linter has something to say. */
  function drawTile(ctx: CanvasRenderingContext2D, f: Frame, n: GNode) {
    const a = nodeAlpha(f, n);
    const isSel = n.id === f.sel;
    const isSim = f.sim.has(n.id);
    const isHov = n.id === f.hovId;
    const priority = isSel || isHov || isSim;

    // Constant screen-size card, positions snapped to the pixel grid — this
    // is what keeps the text and hairlines execution-graph crisp.
    ctx.font = `${n.hub || isSel ? "600 " : "500 "}${(TILE_FONT_PX * sf()).toFixed(2)}px ${MONO}`;
    const caption = n.label;
    const textW = ctx.measureText(caption).width;
    const w = Math.round(textW + 20 * sf());
    const h = Math.round(th());
    const x = Math.round(SX(n.x) - w / 2);
    const y = Math.round(SY(n.y) - h / 2);

    // Collision: a tile that would sit on another tile demotes to a dot —
    // unless it is an interaction state, which always wins its spot.
    const rect = { x: x - 3, y: y - 3, w: w + 6, h: h + 6 };
    if (!priority && tileCollides(f, rect)) {
      drawDot(ctx, f, n);
      return;
    }
    f.placed.push(rect);
    n.labelW = w / camera.k;

    // Tests read as provisional: same card, a quieter presence.
    ctx.globalAlpha = n.kind === "test" ? a * 0.62 : a;

    rrect(ctx, x + 0.5, y + 0.5, w, h, 6 * sf());
    ctx.fillStyle = f.p.card;
    ctx.fill();
    ctx.lineWidth = 1;
    // Lint pulls the whole border, hubs get the bright rim.
    ctx.strokeStyle = n.quality < 80 ? qualityColor(n.quality, f.p) : n.hub ? f.p.text3 : f.p.borderStrong;
    ctx.stroke();

    // The kind accent, worn on the left edge like a MEG category bar. The
    // drill view colours by tag group instead.
    const accent = props.drillFile ? groupColor(n.group, f.p.dark) : kindColor(n.kind, f.p.dark, f.p.text3);
    ctx.save();
    rrect(ctx, x + 0.5, y + 0.5, w, h, 6 * sf());
    ctx.clip();
    ctx.fillStyle = accent;
    ctx.fillRect(x, y, Math.max(2, 3 * sf()), h + 1);
    ctx.restore();

    // Service surface: a leaf dot marks files binding routes or topics.
    if (n.serves.length > 0) {
      ctx.beginPath();
      ctx.arc(x + w - 6 * sf(), y + 6 * sf(), 2.2 * sf(), 0, Math.PI * 2);
      ctx.fillStyle = f.p.leaf;
      ctx.fill();
    }

    tileOutlines(ctx, f, x, y, w, h, isSel, isHov, isSim);

    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    ctx.fillStyle = f.p.tileText;
    ctx.fillText(caption, x + 11 * sf(), y + h / 2 + 0.5);
  }

  function tileOutlines(
    ctx: CanvasRenderingContext2D,
    f: Frame,
    x: number,
    y: number,
    w: number,
    h: number,
    isSel: boolean,
    isHov: boolean,
    isSim: boolean,
  ) {
    if (isSel || isHov) {
      rrect(ctx, x - 2.5, y - 2.5, w + 6, h + 6, 8 * sf());
      ctx.lineWidth = isSel ? 1.5 : 1;
      ctx.strokeStyle = f.p.select;
      ctx.stroke();
    }
    if (isSim) {
      ctx.setLineDash([4, 3]);
      rrect(ctx, x - 4.5, y - 4.5, w + 10, h + 10, 9 * sf());
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = f.p.violet;
      ctx.stroke();
      ctx.setLineDash([]);
    }
  }

  function tileCollides(f: Frame, r: { x: number; y: number; w: number; h: number }): boolean {
    for (const pl of f.placed) {
      if (r.x < pl.x + pl.w && pl.x < r.x + r.w && r.y < pl.y + pl.h && pl.y < r.y + r.h) return true;
    }
    return false;
  }

  // ------------------------------------------------------------ interactions

  function toWorld(sx: number, sy: number): { x: number; y: number } {
    return { x: (sx - camera.tx) / camera.k, y: (sy - camera.ty) / camera.k };
  }

  function hit(sx: number, sy: number): number {
    const { x, y } = toWorld(sx, sy);
    const tol = 5 / camera.k;
    // Tiles on top were drawn last, so they get first claim.
    for (let i = graph.nodes.length - 1; i >= 0; i--) {
      const n = graph.nodes[i]!;
      if (n.labelW <= 0) continue;
      if (Math.abs(x - n.x) <= n.labelW / 2 + tol && Math.abs(y - n.y) <= tileH(n) / 2 + tol) return i;
    }
    for (let i = graph.nodes.length - 1; i >= 0; i--) {
      const n = graph.nodes[i]!;
      if (n.labelW > 0) continue;
      const dx = n.x - x;
      const dy = n.y - y;
      const r = dotR(n) / camera.k + tol;
      if (dx * dx + dy * dy <= r * r) return i;
    }
    return -1;
  }

  function onPointerDown(e: PointerEvent) {
    const start = toLocal(e.clientX, e.clientY);
    const idx = hit(start.x, start.y);
    let last = start;
    let moved = false;
    setHover(null);
    canvas.setPointerCapture(e.pointerId);

    const onMove = (ev: PointerEvent) => {
      const cur = toLocal(ev.clientX, ev.clientY);
      // Forgiving click threshold: a few pixels of hand jitter is still a
      // click, not the start of a drag.
      if (Math.abs(cur.x - start.x) + Math.abs(cur.y - start.y) > 7) moved = true;
      if (!moved) return;
      takeCamera();
      if (idx >= 0) {
        const n = graph.nodes[idx]!;
        const w = toWorld(cur.x, cur.y);
        n.x = w.x;
        n.y = w.y;
        n.pinned = true;
      } else {
        camera.tx += cur.x - last.x;
        camera.ty += cur.y - last.y;
      }
      last = cur;
      draw();
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      if (!moved) props.onSelect(idx >= 0 ? (graph.nodes[idx]?.id ?? null) : null);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    setHover(null);
    const { x: sx, y: sy } = toLocal(e.clientX, e.clientY);
    const before = toWorld(sx, sy);
    camera.k = Math.min(3.2, Math.max(0.05, camera.k * Math.exp(-e.deltaY * 0.0016)));
    camera.tx = sx - before.x * camera.k;
    camera.ty = sy - before.y * camera.k;
    takeCamera();
    syncZoomHint();
    draw();
  }

  function onHoverMove(e: PointerEvent) {
    if (e.buttons) return;
    const { x: sx, y: sy } = toLocal(e.clientX, e.clientY);
    const idx = hit(sx, sy);
    const cur = hover();
    if (idx === -1) {
      if (cur) {
        setHover(null);
        draw();
      }
      return;
    }
    const n = graph.nodes[idx]!;
    if (cur !== n.id) {
      setHover(n.id);
      draw();
    }
  }

  /** Pins the tooltip to its node's *current* position. Runs every draw, so
   * it tracks the simulation, panning and zooming — logical canvas units are
   * converted through the element's real CSS size, which also absorbs the
   * app's UI zoom. */
  function positionTip() {
    const id = hover();
    if (!id || !tipEl) return;
    const idx = graph.byId.get(id);
    const n = idx !== undefined ? graph.nodes[idx] : undefined;
    if (!n) {
      setHover(null);
      return;
    }
    const scale = canvas.offsetWidth / Math.max(1, canvas.width / dprUsed);
    const top = n.labelW > 0 ? tileH(n) / 2 + 2 : n.r;
    tipEl.style.left = `${(n.x * camera.k + camera.tx) * scale}px`;
    tipEl.style.top = `${((n.y - top) * camera.k + camera.ty) * scale - 8}px`;
  }

  function onDblClick(e: MouseEvent) {
    const { x, y } = toLocal(e.clientX, e.clientY);
    const idx = hit(x, y);
    const id = idx >= 0 ? graph.nodes[idx]?.id : undefined;
    if (id) props.onActivate(id);
  }

  onMount(() => {
    const ro = new ResizeObserver(() => resize());
    ro.observe(wrap);
    resize();
    rebuild(props.doc);
    const observer = new MutationObserver(() => draw());
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    onCleanup(() => {
      disposed = true;
      cancelAnimationFrame(rafId);
      ro.disconnect();
      observer.disconnect();
    });
  });

  createEffect(
    on(
      () => [props.doc, props.drillFile, props.groupBase] as const,
      ([d]) => rebuild(d),
      { defer: true },
    ),
  );
  // Selection, similarity and filter all change what draw() shades.
  createEffect(() => {
    void props.selected;
    void props.filter;
    similarByFile();
    if (!disposed) draw();
  });

  const hoverFile = createMemo(() => {
    const h = hover();
    return h ? (props.doc.files.find((f) => f.id === h) ?? null) : null;
  });

  /** Directory → hue rows for the legend, reproducing buildGraph's exact
   * first-seen group order so the swatches always match the discs. Hidden in
   * the drill view, where groups are tags. */
  const legendGroups = createMemo(() => {
    void store.settings(); // re-derive colors when the theme flips
    if (props.drillFile) return [];
    const dark = document.documentElement.dataset.theme !== "light";
    const seen = new Map<string, number>();
    for (const f of props.doc.files) {
      const d = topDirUnder(f.id, props.groupBase);
      if (!seen.has(d)) seen.set(d, seen.size);
    }
    return [...seen.entries()].map(([name, g]) => ({ name, color: groupColor(g, dark) }));
  });

  return (
    <div class="arch-canvas-wrap" ref={wrap}>
      <canvas
        ref={canvas}
        class="arch-canvas"
        onPointerDown={onPointerDown}
        onPointerMove={onHoverMove}
        onPointerLeave={() => setHover(null)}
        onWheel={onWheel}
        onDblClick={onDblClick}
      />
      <Show when={dropped() > 0}>
        <div class="arch-capnote">{dropped()} weakest edges hidden for clarity</div>
      </Show>
      <Show when={legendGroups().length > 1}>
        <div class="arch-groups">
          <For each={legendGroups().slice(0, 12)}>
            {(g) => (
              <button
                type="button"
                class="arch-group-row"
                data-on={props.filter !== "" && g.name.toLowerCase().includes(props.filter)}
                title={`Show only ${g.name}`}
                onClick={() => props.onGroup(g.name)}
              >
                <span class="arch-group-dot" style={{ background: g.color }} />
                <span class="truncate">{g.name}</span>
              </button>
            )}
          </For>
          <Show when={legendGroups().length > 12}>
            <div class="arch-group-row muted">+{legendGroups().length - 12} more</div>
          </Show>
          <Show when={zoomedOut()}>
            <div class="arch-group-row muted arch-zoom-hint">zoom in for names</div>
          </Show>
        </div>
      </Show>
      <Show when={hover() && hoverFile()}>
        {(_) => {
          const f = hoverFile()!;
          queueMicrotask(positionTip);
          return (
            <div class="arch-tip" ref={tipEl} style={{ left: "-9999px", top: "-9999px" }}>
              <div class="arch-tip-name">{f.id}</div>
              <Show when={f.role}>
                <div class="arch-tip-role">{f.role}</div>
              </Show>
              <div class="arch-tip-meta">
                {f.kind ? `${KIND_GLYPHS[f.kind] ?? ""} ${f.kind} · ` : ""}q{f.quality} · {f.functions.length}{" "}
                fns · {f.lines} lines{maxCxOf(f) >= 8 ? ` · cx ${maxCxOf(f)}` : ""}
              </div>
              <Show when={(f.provides ?? []).length > 0}>
                <div class="arch-tip-meta" style={{ color: "var(--leaf-400)" }}>
                  serves {(f.provides ?? []).slice(0, 3).join(" · ")}
                </div>
              </Show>
            </div>
          );
        }}
      </Show>
    </div>
  );
}

// ------------------------------------------------------------- detail panel

interface PanelProps {
  doc: ArchDoc;
  file: ArchFile | null;
  selectedFn: string | null;
  similar: SimilarHit[];
  filter: string;
  onSelectFile: (id: string | null) => void;
  onSelectFn: (fileId: string, fn: string) => void;
  onClose: () => void;
}

function ArchPanel(props: PanelProps) {
  const [fnSort, setFnSort] = createSignal<"line" | "cx">("line");
  const usedByList = createMemo(() => {
    const f = props.file;
    if (!f) return [];
    // Persisted by the scan; the edge filter covers pre-upgrade maps.
    if ((f.usedBy ?? []).length > 0) return f.usedBy;
    return props.doc.edges.filter((e) => e.to === f.id).map((e) => e.from);
  });
  const usedBy = () => usedByList().length;

  return (
    <aside class="arch-panel">
      <Show
        when={props.file}
        fallback={
          <Overview doc={props.doc} onSelectFile={props.onSelectFile} onSelectFn={props.onSelectFn} />
        }
      >
        {(file) => (
          <>
            <div class="arch-panel-head">
              <button
                type="button"
                class="arch-panel-title truncate"
                title={`Open ${file().id}`}
                onClick={() => void openPath(`${props.doc.root}/${file().id}`)}
              >
                <FileIcon size={12} />
                <span class="truncate">{file().id}</span>
              </button>
              <QualityChip q={file().quality} />
              <button type="button" class="btn" data-size="icon-sm" title="Deselect" onClick={props.onClose}>
                <Close size={10} />
              </button>
            </div>

            <div class="arch-panel-scroll">
              <Show when={file().role}>
                <p class="arch-role">{file().role}</p>
              </Show>
              <div class="arch-meta muted">
                {file().kind ? `${KIND_GLYPHS[file().kind ?? ""] ?? ""} ${file().kind} · ` : ""}
                {file().lang} · {file().lines} lines · uses {file().imports.length} · used by {usedBy()}
              </div>

              {/* Execution flow: one hop upstream and downstream, clickable to
                  walk the dependency chain without touching the canvas. */}
              <Show when={file().imports.length > 0}>
                <div class="arch-fn-head muted" style={{ color: "var(--accent)" }}>
                  → uses
                </div>
                <For each={file().imports.slice(0, 12)}>
                  {(dep) => (
                    <button type="button" class="arch-listrow" onClick={() => props.onSelectFile(dep)}>
                      <span class="truncate">{dep}</span>
                    </button>
                  )}
                </For>
                <Show when={file().imports.length > 12}>
                  <div class="arch-none muted">+{file().imports.length - 12} more</div>
                </Show>
              </Show>
              <Show when={usedByList().length > 0}>
                <div class="arch-fn-head muted" style={{ color: "var(--steel-400)" }}>
                  → used by
                </div>
                <For each={usedByList().slice(0, 12)}>
                  {(dep) => (
                    <button type="button" class="arch-listrow" onClick={() => props.onSelectFile(dep)}>
                      <span class="truncate">{dep}</span>
                    </button>
                  )}
                </For>
                <Show when={usedByList().length > 12}>
                  <div class="arch-none muted">+{usedByList().length - 12} more</div>
                </Show>
              </Show>
              <Show when={file().lints.length > 0}>
                <div class="arch-lints">
                  <For each={file().lints}>
                    {(l) => (
                      <span class="chip" data-tone="warn">
                        {l}
                      </span>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={file().log.length > 0}>
                <details class="arch-log">
                  <summary>
                    <History size={10} /> file history
                  </summary>
                  <For each={[...file().log].reverse()}>
                    {(l) => (
                      <div class="arch-log-row">
                        <span>{l.event}</span>
                        <span class="muted tabular">{formatWhen(l.ts)}</span>
                      </div>
                    )}
                  </For>
                </details>
              </Show>

              <div class="arch-fn-head muted arch-fn-headrow">
                <span>functions</span>
                <div class="spacer" />
                <button
                  type="button"
                  class="arch-sort"
                  data-on={fnSort() === "cx"}
                  title="Sort by complexity — the optimisation queue"
                  onClick={() => setFnSort(fnSort() === "cx" ? "line" : "cx")}
                >
                  {fnSort() === "cx" ? "cx ↓" : "by line"}
                </button>
              </div>
              <For
                each={sortFns(
                  file().functions.filter(
                    (f) => !props.filter || f.name.toLowerCase().includes(props.filter),
                  ),
                  fnSort(),
                )}
                fallback={<div class="muted arch-none">No functions detected.</div>}
              >
                {(fn) => (
                  <FnRow
                    fileId={file().id}
                    fn={fn}
                    selected={props.selectedFn === fn.name}
                    similar={props.similar}
                    onSelect={() => props.onSelectFn(file().id, fn.name)}
                    onJump={props.onSelectFn}
                    root={props.doc.root}
                  />
                )}
              </For>
            </div>
          </>
        )}
      </Show>
    </aside>
  );
}

function Overview(props: {
  doc: ArchDoc;
  onSelectFile: (id: string) => void;
  onSelectFn: (fileId: string, fn: string) => void;
}) {
  const hubs = createMemo(() => [...props.doc.files].sort((a, b) => b.inDegree - a.inDegree).slice(0, 6));
  const risky = createMemo(() =>
    [...props.doc.files]
      .filter((f) => f.functions.length > 0)
      .sort((a, b) => a.quality - b.quality)
      .slice(0, 6),
  );
  const complex = createMemo(() => {
    const out: { fileId: string; fn: ArchFn }[] = [];
    for (const f of props.doc.files) {
      for (const fn of f.functions) {
        if (fn.complexity >= 8) out.push({ fileId: f.id, fn });
      }
    }
    return out.sort((a, b) => b.fn.complexity - a.fn.complexity).slice(0, 6);
  });
  const deadFns = createMemo(() => {
    const out: { fileId: string; fn: ArchFn }[] = [];
    for (const f of props.doc.files) {
      for (const fn of f.functions) {
        if (fn.dead) out.push({ fileId: f.id, fn });
      }
    }
    return out.slice(0, 10);
  });
  return (
    <div class="arch-panel-scroll">
      <div class="arch-fn-head muted">most connected</div>
      <For each={hubs()}>
        {(f) => (
          <button type="button" class="arch-listrow" onClick={() => props.onSelectFile(f.id)}>
            <span class="truncate">{f.id}</span>
            <span class="muted tabular">{f.inDegree}</span>
          </button>
        )}
      </For>
      <div class="arch-fn-head muted">needs attention</div>
      <For each={risky()}>
        {(f) => (
          <button type="button" class="arch-listrow" onClick={() => props.onSelectFile(f.id)}>
            <span class="truncate">{f.id}</span>
            <QualityChip q={f.quality} />
          </button>
        )}
      </For>
      <Show when={complex().length > 0}>
        <div class="arch-fn-head muted" style={{ color: "var(--warn)" }}>
          most complex
        </div>
        <For each={complex()}>
          {(d) => (
            <button
              type="button"
              class="arch-listrow"
              title={`${d.fileId} — start optimising here`}
              onClick={() => props.onSelectFn(d.fileId, d.fn.name)}
            >
              <span class="truncate">
                {d.fn.name} <span class="muted">· {d.fileId.split("/").pop()}</span>
              </span>
              <Show when={d.fn.bigO}>
                {(t) => (
                  <span class="arch-cx arch-bigo" data-heat={oHeat(t())}>
                    {t()}
                  </span>
                )}
              </Show>
              <span class="arch-cx tabular" data-heat={cxHeat(d.fn.complexity)}>
                cx {d.fn.complexity}
              </span>
            </button>
          )}
        </For>
      </Show>
      <Show when={deadFns().length > 0}>
        <div class="arch-fn-head muted" style={{ color: "var(--danger)" }}>
          possibly dead code
        </div>
        <For each={deadFns()}>
          {(d) => (
            <button
              type="button"
              class="arch-listrow"
              title={`${d.fileId} — no references found`}
              onClick={() => props.onSelectFile(d.fileId)}
            >
              <span class="truncate">
                {d.fn.name} <span class="muted">· {d.fileId.split("/").pop()}</span>
              </span>
              <span class="chip" data-tone="danger">
                unused
              </span>
            </button>
          )}
        </For>
      </Show>
      <p class="arch-hint muted">
        Click a node for its functions. Click a function to light up similar code across the map.
      </p>
    </div>
  );
}

function QualityChip(props: { q: number }) {
  const tone = () => (props.q >= 80 ? "ok" : props.q >= 60 ? "warn" : "danger");
  return (
    <span class="chip" data-tone={tone()} title="Code quality from lint heuristics">
      q{props.q}
    </span>
  );
}

interface FnRowProps {
  fileId: string;
  fn: ArchFn;
  selected: boolean;
  similar: SimilarHit[];
  root: string;
  onSelect: () => void;
  onJump: (fileId: string, fn: string) => void;
}

function FnRow(props: FnRowProps) {
  const f = () => props.fn;
  const qcolor = () =>
    f().quality >= 80 ? "var(--ok)" : f().quality >= 60 ? "var(--warn)" : "var(--danger)";

  return (
    <div class="arch-fn" data-selected={props.selected}>
      <button type="button" class="arch-fn-row" onClick={props.onSelect}>
        <span class="arch-fn-dot" style={{ background: qcolor() }} title={`q${f().quality}`} />
        <span class="arch-fn-name truncate">{f().name}</span>
        <Show when={f().core}>
          <span class="chip" data-tone="accent" title="Referenced across several files">
            core
          </span>
        </Show>
        <Show when={f().dead}>
          <span class="chip" data-tone="danger" title="No reference to this function anywhere in the scan">
            unused
          </span>
        </Show>
        <Show when={f().tag && f().tag !== "core"}>
          <span class="chip" data-kind={f().tag ?? undefined}>
            {f().tag}
          </span>
        </Show>
        <Show when={f().stale}>
          <span class="chip" title="Body changed since last described">
            ~
          </span>
        </Show>
        <Show when={f().bigO}>
          {(t) => (
            <span
              class="arch-cx arch-bigo"
              data-heat={oHeat(t())}
              title={`Model-estimated time complexity${f().space ? ` · space ${f().space}` : ""}`}
            >
              {t()}
            </span>
          )}
        </Show>
        <span
          class="arch-cx tabular"
          data-heat={cxHeat(f().complexity)}
          title="Branching complexity — higher is harder to keep correct"
        >
          cx {f().complexity}
        </span>
      </button>
      <Show when={f().desc}>
        <div class="arch-fn-desc muted">{f().desc}</div>
      </Show>

      <Show when={props.selected}>
        <div class="arch-fn-detail">
          <div class="arch-fn-sig selectable">
            {f().name}({f().params})
          </div>
          <Show when={f().inputs || f().outputs}>
            <div class="arch-io">
              <Show when={f().inputs}>
                <span class="arch-io-k">in</span>
                <span>{f().inputs}</span>
              </Show>
              <Show when={f().outputs}>
                <span class="arch-io-k">out</span>
                <span>{f().outputs}</span>
              </Show>
            </div>
          </Show>
          <Show when={f().note}>
            <div class="arch-note" data-kind={f().tag ?? "math"}>
              {f().tag === "routing" ? "⇋" : "∑"} {f().note}
            </div>
          </Show>
          <Show when={f().lints.length > 0}>
            <div class="arch-lints">
              <For each={f().lints}>
                {(l) => (
                  <span class="chip" data-tone="warn">
                    {l}
                  </span>
                )}
              </For>
            </div>
          </Show>
          <div class="arch-fn-actions">
            <button
              type="button"
              class="btn"
              data-size="sm"
              onClick={() => void openPath(`${props.root}/${props.fileId}`, f().line)}
            >
              <FileIcon size={11} /> line {f().line}
            </button>
            <span class="muted tabular" style={{ "font-size": "var(--fs-xs)" }}>
              cx {f().complexity} · depth {f().nesting}
              {f().bigO ? ` · time ${f().bigO}` : ""}
              {f().space ? ` · space ${f().space}` : ""}
            </span>
          </div>

          <Show when={f().log.length > 0}>
            <details class="arch-log" open>
              <summary>
                <History size={10} /> history
              </summary>
              <For each={[...f().log].reverse()}>
                {(l) => (
                  <div class="arch-log-row">
                    <span>{l.event}</span>
                    <span class="muted tabular">{formatWhen(l.ts)}</span>
                  </div>
                )}
              </For>
            </details>
          </Show>

          <Show when={props.similar.length > 0}>
            <div class="arch-fn-head muted">similar elsewhere</div>
            <For each={props.similar}>
              {(s) => (
                <button
                  type="button"
                  class="arch-listrow"
                  data-sim="true"
                  title={`${s.fileId} · ${Math.round(s.score * 100)}% similar`}
                  onClick={() => props.onJump(s.fileId, s.fn.name)}
                >
                  <span class="truncate">
                    {s.fn.name} <span class="muted">· {s.fileId.split("/").pop()}</span>
                  </span>
                  <span class="arch-sim-score tabular">{Math.round(s.score * 100)}%</span>
                </button>
              )}
            </For>
          </Show>
        </div>
      </Show>
    </div>
  );
}
