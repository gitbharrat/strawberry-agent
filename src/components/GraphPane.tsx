// The Model Execution Graph pane: a Fusion-style node canvas.
//
// Tiles are DOM elements and wires are one SVG overlay, both inside a single
// transformed world layer — richer nodes than a canvas could give, at a scale
// (tens of nodes, not thousands) where the DOM wins. Groups collapse a
// subgraph into one tile the way Fusion macros do; saved custom nodes are
// just fragments that stamp in pre-grouped.

import { batch, createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import { Close, NodeGraph, Pin, Play, Stop, Trash } from "./Icons";
import { Markdown } from "./Markdown";
import { ModelPicker } from "./ModelPicker";
import { api, Channel } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { getSession } from "~/lib/session";
import { megState } from "~/lib/megState";
import { formatCost, formatDuration, formatTokens } from "~/lib/actions";
import { openSketchForNode } from "./SketchPad";
import {
  CATEGORY_COLOR,
  designBlockOf,
  hasInput,
  makeNode,
  materializePlan,
  NODE_TYPE,
  NODE_TYPES,
  nodeId,
  outPorts,
  parseTheme,
  parseWireframe,
  TEMPLATES,
  updateDesignJson,
  type CustomNodeDef,
  type DesignJson,
  type WireArea,
  validateSpec,
  wouldCycle,
  type GraphNode,
  type GraphSpec,
  type NodeArtifact,
  type NodeKind,
  type NodeRunStatus,
  type OutPort,
  type Verdict,
} from "~/lib/meg";
import type { AgentEvent } from "~/lib/types";
import { errText } from "~/lib/err";

const TILE_W = 150;
const TILE_H = 36;
const GROUP_H = 44;

interface NodeLive {
  status: NodeRunStatus;
  detail: string | null;
  text: string;
}

interface RunLive {
  runId: string | null;
  nodes: Record<string, NodeLive>;
  loops: Record<string, { i: number; max: number }>;
}

const emptySpec = (): GraphSpec => ({ id: "", name: "", nodes: [], edges: [], groups: [] });

function portOut(n: GraphNode, port: OutPort): { x: number; y: number } {
  const ports = outPorts(n);
  if (ports.length < 2) return { x: n.x + TILE_W, y: n.y + TILE_H / 2 };
  const i = Math.max(0, ports.indexOf(port));
  return { x: n.x + TILE_W, y: n.y + TILE_H * (i === 0 ? 0.3 : 0.72) };
}

function portIn(n: GraphNode): { x: number; y: number } {
  return { x: n.x, y: n.y + TILE_H / 2 };
}

/** The xyflow bezier: control points pushed along the flow axis, with a
 * gentle square-root flare when the target sits behind the source (loops). */
function wirePath(sx: number, sy: number, tx: number, ty: number): string {
  const d = tx - sx;
  const off = d >= 0 ? Math.max(24, d * 0.5) : 25 * Math.sqrt(-d) * 0.6 + 30;
  return `M${sx},${sy} C${sx + off},${sy} ${tx - off},${ty} ${tx},${ty}`;
}

function wireMid(sx: number, sy: number, tx: number, ty: number): { x: number; y: number } {
  const d = tx - sx;
  const off = d >= 0 ? Math.max(24, d * 0.5) : 25 * Math.sqrt(-d) * 0.6 + 30;
  // Cubic at t = 0.5: (p0 + 3p1 + 3p2 + p3) / 8.
  return { x: (sx + 3 * (sx + off) + 3 * (tx - off) + tx) / 8, y: (sy + 3 * sy + 3 * ty + ty) / 8 };
}

function shortModel(id: string): string {
  const tail = id.split("/").pop() ?? id;
  return tail.length > 20 ? `${tail.slice(0, 19)}…` : tail;
}

function loadCustoms(): CustomNodeDef[] {
  try {
    const raw = localStorage.getItem("sa.meg.custom");
    return raw ? (JSON.parse(raw) as CustomNodeDef[]) : [];
  } catch {
    return [];
  }
}

export function GraphPane() {
  const chatId = () => store.activeChatId();

  // ---- which graph is open
  const [selectedId, setSelectedId] = createSignal<string | null>(restore("sa.meg.sel"));
  const doc = createMemo(() => megState.graphs().find((g) => g.id === selectedId()) ?? null);

  // ---- the editable spec (source of truth while the pane is open)
  const [spec, setSpec] = createStore<GraphSpec>(emptySpec());
  const [arts, setArts] = createSignal<Record<string, NodeArtifact>>({});

  // ---- selection and interaction
  const [sel, setSel] = createSignal<ReadonlySet<string>>(new Set<string>());
  const [selEdge, setSelEdge] = createSignal<string | null>(null);
  const [cam, setCam] = createSignal({ k: 1, tx: 40, ty: 60 });
  const [wire, setWire] = createSignal<{ from: string; port: OutPort; x: number; y: number } | null>(null);
  const [palette, setPalette] = createSignal<{
    sx: number;
    sy: number;
    wx: number;
    wy: number;
    connectFrom?: { node: string; port: OutPort };
  } | null>(null);
  const [marquee, setMarquee] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [menu, setMenu] = createSignal<{ sx: number; sy: number; node?: string; group?: string } | null>(
    null,
  );
  const [namePrompt, setNamePrompt] = createSignal<{ sx: number; sy: number; ids: string[] } | null>(null);
  const [customs, setCustoms] = createSignal<CustomNodeDef[]>(loadCustoms());

  // ---- live run state
  const [run, setRun] = createStore<RunLive>({ runId: null, nodes: {}, loops: {} });
  const [runInput, setRunInput] = createSignal("");

  // ---- auto-planning: one model call drafts the graph, the user reviews it.
  const [planning, setPlanning] = createSignal(false);
  const [draft, setDraft] = createSignal<{ rationale: string; goal: string } | null>(null);

  let wrap!: HTMLDivElement;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  function restore(key: string): string | null {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  }

  function pickGraph(id: string) {
    setSelectedId(id);
    try {
      localStorage.setItem("sa.meg.sel", id);
    } catch {
      /* private mode */
    }
  }

  function persistCustoms(next: CustomNodeDef[]) {
    setCustoms(next);
    try {
      localStorage.setItem("sa.meg.custom", JSON.stringify(next));
    } catch {
      /* private mode */
    }
  }

  // The webview's own context menu must never appear over the canvas: a
  // native listener in the capture phase beats both Solid's delegation and
  // the focused palette input (whose edit menu is what WKWebView shows).
  onMount(() => {
    const suppress = (e: MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const el = e.target as HTMLElement | null;
      if (el?.closest(".meg-palette") || el?.closest(".meg-menu") || el?.closest(".meg-name-prompt")) return;
      const s = toLocal(e.clientX, e.clientY);
      setPalette(null);
      const groupEl = el?.closest("[data-group-id]") as HTMLElement | null;
      if (groupEl?.dataset.groupId) {
        setMenu({ sx: s.x, sy: s.y, group: groupEl.dataset.groupId });
        return;
      }
      if (el?.closest("[data-node-id]")) return; // the tile's own menu handles it
      setMenu(null);
      openPalette(s.x, s.y);
    };
    wrap.addEventListener("contextmenu", suppress, { capture: true });
    onCleanup(() => wrap.removeEventListener("contextmenu", suppress, { capture: true }));
  });

  // First open: load the library, and seed the simplest template if empty.
  onMount(() => {
    void (async () => {
      try {
        if (!megState.loaded()) await megState.load();
        if (megState.graphs().length === 0) {
          const tplSpec = TEMPLATES[0]!.build();
          const saved = await api.graphSave({
            projectId: store.activeProject()?.id ?? null,
            name: tplSpec.name,
            spec: tplSpec,
          });
          megState.upsert(saved);
          pickGraph(saved.id);
        } else if (!doc()) {
          pickGraph(megState.graphs()[0]!.id);
        }
      } catch (e) {
        toast("error", errText(e));
      }
    })();
  });

  // Adopt the stored doc whenever the selected graph changes.
  createEffect(
    on(doc, (d) => {
      if (!d) return;
      if (d.spec.id !== spec.id) {
        const s = structuredClone(unwrap(d.spec)) as GraphSpec;
        // Specs saved before groups existed load without the field.
        if (!s.groups) s.groups = [];
        for (const n of s.nodes) if (n.group === undefined) n.group = null;
        setSpec(reconcile(s));
        setSel(new Set<string>());
        setSelEdge(null);
        queueMicrotask(fit);
      }
      setArts(d.artifacts);
    }),
  );

  /** All spec mutations funnel through here so persistence cannot be missed. */
  function mutate(fn: () => void) {
    batch(fn);
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void save(), 700);
  }

  async function save() {
    const d = doc();
    if (!d || !spec.id) return;
    try {
      const saved = await api.graphSave({
        id: d.id,
        projectId: d.projectId,
        name: spec.name,
        spec: structuredClone(unwrap(spec)),
      });
      megState.upsert(saved);
    } catch (e) {
      toast("error", `Could not save the graph: ${errText(e)}`);
    }
  }
  onCleanup(() => {
    if (saveTimer) {
      clearTimeout(saveTimer);
      void save();
    }
  });

  const problems = createMemo(() => validateSpec(spec));
  const nodeById = (id: string) => spec.nodes.find((n) => n.id === id);

  /** Persists a hand-edited artifact (the design editor's writes) and pins
   * the node so a re-run serves the edit instead of overwriting it. */
  async function editArtifact(nodeId: string, content: string) {
    if (!spec.id) return;
    try {
      const doc = await api.graphSetArtifact(spec.id, nodeId, content);
      setArts(doc.artifacts);
      if (!nodeById(nodeId)?.pinned) {
        setSpec("nodes", (n) => n.id === nodeId, { pinned: true });
      }
    } catch (e) {
      toast("error", errText(e));
    }
  }

  // ------------------------------------------------------------------ groups

  /** Collapsed groups with their members and the tile rectangle they render as. */
  const collapsedGroups = createMemo(() => {
    const out: { id: string; label: string; x: number; y: number; members: GraphNode[] }[] = [];
    for (const g of spec.groups) {
      if (!g.collapsed) continue;
      const members = spec.nodes.filter((n) => n.group === g.id);
      if (members.length === 0) continue;
      const x = Math.min(...members.map((m) => m.x));
      const y = Math.min(...members.map((m) => m.y));
      out.push({ id: g.id, label: g.label, x, y, members });
    }
    return out;
  });

  /** node id → its collapsed group's tile rect, for hiding and wire routing. */
  const collapsedOf = createMemo(() => {
    const m = new Map<string, { x: number; y: number; group: string }>();
    for (const g of collapsedGroups()) {
      for (const member of g.members) m.set(member.id, { x: g.x, y: g.y, group: g.id });
    }
    return m;
  });

  const expandedGroups = createMemo(() =>
    spec.groups
      .filter((g) => !g.collapsed)
      .map((g) => {
        const members = spec.nodes.filter((n) => n.group === g.id);
        if (members.length === 0) return null;
        const pad = 18;
        const x = Math.min(...members.map((m) => m.x)) - pad;
        const y = Math.min(...members.map((m) => m.y)) - pad - 14;
        const w = Math.max(...members.map((m) => m.x + TILE_W)) - x + pad;
        const h = Math.max(...members.map((m) => m.y + TILE_H)) - y + pad;
        return { id: g.id, label: g.label, x, y, w, h };
      })
      .filter((g): g is NonNullable<typeof g> => g !== null),
  );

  function outAnchor(n: GraphNode, port: OutPort): { x: number; y: number } {
    const g = collapsedOf().get(n.id);
    return g ? { x: g.x + TILE_W, y: g.y + GROUP_H / 2 } : portOut(n, port);
  }
  function inAnchor(n: GraphNode): { x: number; y: number } {
    const g = collapsedOf().get(n.id);
    return g ? { x: g.x, y: g.y + GROUP_H / 2 } : portIn(n);
  }

  function groupSelection() {
    const ids = [...sel()].filter((id) => nodeById(id));
    if (ids.length < 2) {
      toast("info", "Select at least two nodes to group them.");
      return;
    }
    const gid = nodeId();
    mutate(() => {
      setSpec("groups", (gs) => [...gs, { id: gid, label: "Group", collapsed: true }]);
      for (const id of ids) setSpec("nodes", (n) => n.id === id, "group", gid);
    });
    setSel(new Set<string>());
  }

  function ungroup(gid: string) {
    mutate(() => {
      setSpec(
        "nodes",
        (n) => n.group === gid,
        "group",
        null as unknown as string, // store setter: null clears membership
      );
      setSpec("groups", (gs) => gs.filter((g) => g.id !== gid));
    });
  }

  function setGroupCollapsed(gid: string, collapsed: boolean) {
    mutate(() => setSpec("groups", (g) => g.id === gid, "collapsed", collapsed));
    setSel(new Set<string>());
  }

  /** The member a wire dropped on the group tile should connect to: the one
   * with no feed from inside the group — its natural entry. */
  function groupEntry(gid: string): GraphNode | undefined {
    const members = spec.nodes.filter((n) => n.group === gid);
    const internal = new Set(
      spec.edges.filter((e) => !e.loop && members.some((m) => m.id === e.from)).map((e) => e.to),
    );
    return members.find((m) => !internal.has(m.id) && hasInput(m)) ?? members[0];
  }

  // ------------------------------------------------------------ custom nodes

  function saveCustom(name: string, ids: string[]) {
    const members = ids.map((id) => nodeById(id)).filter((n): n is GraphNode => !!n && n.kind !== "note");
    if (members.length === 0) return;
    const minX = Math.min(...members.map((m) => m.x));
    const minY = Math.min(...members.map((m) => m.y));
    const idSet = new Set(members.map((m) => m.id));
    const def: CustomNodeDef = {
      name: name.trim() || "Custom node",
      nodes: members.map((m) => ({
        ...structuredClone(unwrap(m)),
        x: m.x - minX,
        y: m.y - minY,
        group: null,
      })),
      edges: spec.edges
        .filter((e) => idSet.has(e.from) && idSet.has(e.to))
        .map((e) => structuredClone(unwrap(e))),
    };
    persistCustoms([...customs().filter((c) => c.name !== def.name), def]);
    toast("ok", `"${def.name}" saved — it is in the add menu now.`);
  }

  function stampCustom(def: CustomNodeDef, wx: number, wy: number) {
    const idMap = new Map<string, string>();
    const gid = nodeId();
    const nodes = def.nodes.map((n) => {
      const fresh = nodeId();
      idMap.set(n.id, fresh);
      return {
        ...structuredClone(n),
        id: fresh,
        x: Math.round(wx + n.x),
        y: Math.round(wy + n.y),
        group: gid,
      };
    });
    const edges = def.edges.map((e) => ({
      ...structuredClone(e),
      id: nodeId(),
      from: idMap.get(e.from) ?? e.from,
      to: idMap.get(e.to) ?? e.to,
    }));
    mutate(() => {
      setSpec("nodes", (ns) => [...ns, ...nodes]);
      setSpec("edges", (es) => [...es, ...edges]);
      setSpec("groups", (gs) => [...gs, { id: gid, label: def.name, collapsed: true }]);
    });
  }

  // ------------------------------------------------------------ run plumbing

  function resetRun(runId: string | null) {
    setRun(
      reconcile({
        runId,
        nodes: Object.fromEntries(
          spec.nodes
            .filter((n) => n.kind !== "note")
            .map((n) => [n.id, { status: "pending" as NodeRunStatus, detail: null, text: "" }]),
        ),
        loops: {},
      }),
    );
  }

  function applyGraphEvent(ev: AgentEvent) {
    switch (ev.type) {
      case "graphStarted":
        if (ev.graphId !== selectedId() && megState.graphs().some((g) => g.id === ev.graphId)) {
          pickGraph(ev.graphId);
        }
        resetRun(run.runId);
        break;
      case "graphNode":
        setRun("nodes", ev.nodeId, (prev) => ({
          status: ev.status as NodeRunStatus,
          detail: ev.detail ?? (ev.status === "running" ? (prev?.detail ?? null) : null),
          text: prev?.text ?? "",
        }));
        break;
      case "graphNodeDelta":
        setRun("nodes", ev.nodeId, "text", (t) => (t ?? "") + ev.text);
        break;
      case "graphNodeDone": {
        const artifact = ev.artifact as NodeArtifact | null;
        setRun("nodes", ev.nodeId, {
          status: ev.status as NodeRunStatus,
          detail: null,
          text: artifact?.content ?? "",
        });
        if (artifact) setArts((a) => ({ ...a, [ev.nodeId]: artifact }));
        break;
      }
      case "graphLoop":
        setRun("loops", ev.edgeId, { i: ev.iteration, max: ev.maxIters });
        break;
      case "runFinished": {
        setRun("runId", null);
        // A successful refinement is curated work: pin it so the next full
        // run serves it instead of overwriting it.
        if (refining) {
          const nid = refining;
          refining = null;
          if (!ev.error) {
            const n = nodeById(nid);
            if (n && !n.pinned) {
              patchNode(nid, { pinned: true });
              toast("ok", "Refined and pinned — unpin to let full runs replace it.");
            }
          }
        }
        // The executor persisted artifacts and messages; pull them in.
        void megState.load();
        break;
      }
      default:
        break;
    }
  }

  function channel(): Channel<AgentEvent> {
    const ch = new Channel<AgentEvent>();
    ch.onmessage = applyGraphEvent;
    return ch;
  }

  // A run started from the composer (bound chat): tap its event stream too.
  let tapped: string | null = null;
  createEffect(() => {
    const cid = chatId();
    if (!cid) return;
    const rid = getSession(cid).live.runId;
    if (rid && rid !== tapped && rid !== run.runId) {
      tapped = rid;
      setRun("runId", rid);
      void api.subscribeRun(rid, channel());
    }
  });

  const busy = () => run.runId !== null;

  async function runGraph() {
    const cid = chatId();
    const d = doc();
    if (!cid || !d || busy()) return;
    if (problems().length > 0) {
      toast("warn", problems()[0]!);
      return;
    }
    await save();
    const session = getSession(cid);
    const text =
      runInput().trim() ||
      [...session.messages].reverse().find((m) => m.role === "user" && m.content.trim())?.content ||
      "";
    if (!text) {
      toast("warn", "Type what the graph should do first — or send a message from the chat.");
      return;
    }
    try {
      resetRun(null);
      const rid = await api.graphRun(cid, d.id, text, channel());
      setRun("runId", rid);
      setRunInput("");
      store.markRunning(cid, true);
      void session.load();
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function stopRun() {
    const rid = run.runId;
    if (rid) await api.cancelRun(rid).catch(() => {});
  }

  /** Asks the model to design a graph for the current goal, saves it as a new
   * graph, and opens it for review — nothing runs until the user says so. */
  async function autoPlan() {
    const cid = chatId();
    if (!cid || planning()) return;
    const session = getSession(cid);
    const goal =
      runInput().trim() ||
      [...session.messages].reverse().find((m) => m.role === "user" && m.content.trim())?.content ||
      "";
    if (!goal) {
      toast("warn", "Type the goal in the run box first — the planner designs the graph for it.");
      return;
    }
    setPlanning(true);
    setDraft(null);
    try {
      const plan = await api.graphPlan(
        cid,
        goal,
        customs().map((c) => ({
          name: c.name,
          stages: c.nodes.map((n) => NODE_TYPE[n.kind]?.title ?? n.kind),
        })),
      );
      const planned = materializePlan(plan, customs());
      if (!planned) {
        toast("error", "The planner returned nothing usable — try rephrasing the goal.");
        return;
      }
      const saved = await api.graphSave({
        projectId: store.activeProject()?.id ?? null,
        name: planned.name,
        spec: planned,
      });
      megState.upsert(saved);
      pickGraph(saved.id);
      setRunInput(goal);
      setDraft({
        rationale: typeof plan.rationale === "string" ? plan.rationale : "",
        goal,
      });
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setPlanning(false);
    }
  }

  /** The sketch node's own UI: open the pad for it. An upstream design's
   * wireframe seeds an empty canvas; an existing drawing is kept. */
  function openPadForSketchNode(node: GraphNode) {
    const d = doc();
    if (!d) return;
    let seedJson: string | null = null;
    for (const e of spec.edges) {
      if (e.loop || e.to !== node.id) continue;
      const src = nodeById(e.from);
      const art = arts()[e.from];
      if (src?.kind === "design" && art?.content) {
        seedJson = designBlockOf(art.content);
        if (seedJson) break;
      }
    }
    openSketchForNode({ graphId: d.id, nodeId: node.id, label: node.label }, seedJson);
  }

  // The node currently being refined, so its result can be auto-pinned.
  let refining: string | null = null;

  /** One-node rework under a user command — never touches the rest. */
  async function refineNode(nid: string, instruction: string) {
    const cid = chatId();
    const d = doc();
    if (!cid || !d || busy()) return;
    try {
      const rid = await api.graphRefine(cid, d.id, nid, instruction, channel());
      refining = nid;
      setRun("runId", rid);
      setRun("nodes", nid, { status: "running", detail: "refining", text: "" });
    } catch (e) {
      toast("error", errText(e));
    }
  }

  // ------------------------------------------------------------- camera math

  function toLocal(cx: number, cy: number): { x: number; y: number } {
    const r = wrap.getBoundingClientRect();
    return { x: cx - r.left, y: cy - r.top };
  }
  function toWorld(sx: number, sy: number): { x: number; y: number } {
    const c = cam();
    return { x: (sx - c.tx) / c.k, y: (sy - c.ty) / c.k };
  }

  /** Opens the add palette clamped inside the canvas, placing nodes where the
   * gesture happened even when the panel itself had to shift. */
  function openPalette(sx: number, sy: number, connectFrom?: { node: string; port: OutPort }) {
    const r = wrap.getBoundingClientRect();
    const w = toWorld(sx, sy);
    setMenu(null);
    setPalette({
      sx: Math.max(0, Math.min(sx, r.width - 300)),
      sy: Math.max(0, Math.min(sy, r.height - 360)),
      wx: w.x,
      wy: w.y,
      connectFrom,
    });
  }

  function onWheel(e: WheelEvent) {
    // Overlays scroll their own lists; the canvas zooms.
    const el = e.target as HTMLElement | null;
    if (el?.closest(".meg-palette") || el?.closest(".meg-menu") || el?.closest(".meg-name-prompt")) return;
    e.preventDefault();
    const { x: sx, y: sy } = toLocal(e.clientX, e.clientY);
    const c = cam();
    const k = Math.min(2.5, Math.max(0.2, c.k * Math.exp(-e.deltaY * 0.0016)));
    setCam({ k, tx: sx - ((sx - c.tx) / c.k) * k, ty: sy - ((sy - c.ty) / c.k) * k });
  }

  function fit() {
    if (spec.nodes.length === 0 || !wrap) return;
    const r = wrap.getBoundingClientRect();
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of spec.nodes) {
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + TILE_W);
      maxY = Math.max(maxY, n.y + TILE_H);
    }
    const pad = 60;
    const k = Math.min(
      1.4,
      Math.max(0.25, Math.min(r.width / (maxX - minX + pad * 2), r.height / (maxY - minY + pad * 2))),
    );
    setCam({
      k,
      tx: r.width / 2 - ((minX + maxX) / 2) * k,
      ty: r.height / 2 - ((minY + maxY) / 2) * k,
    });
  }

  // -------------------------------------------------------------- mutations

  function addNode(kind: NodeKind, wx: number, wy: number, connectFrom?: { node: string; port: OutPort }) {
    const node = makeNode(kind, Math.round(wx - TILE_W / 2), Math.round(wy - TILE_H / 2));
    mutate(() => {
      setSpec("nodes", (ns) => [...ns, node]);
      if (connectFrom && hasInput(node)) {
        setSpec("edges", (es) => [
          ...es,
          {
            id: nodeId(),
            from: connectFrom.node,
            fromPort: connectFrom.port,
            to: node.id,
            loop: false,
            maxIters: 3,
          },
        ]);
      }
    });
    setSel(new Set([node.id]));
    setSelEdge(null);
  }

  function connect(from: string, port: OutPort, to: string) {
    if (from === to) return;
    const target = nodeById(to);
    if (!target || !hasInput(target)) return;
    if (spec.edges.some((e) => e.from === from && e.to === to && e.fromPort === port)) return;
    const loop = wouldCycle(spec, from, to);
    mutate(() => {
      setSpec("edges", (es) => [...es, { id: nodeId(), from, fromPort: port, to, loop, maxIters: 3 }]);
    });
    if (loop) toast("info", "That arrow points back up the flow — it became a loop (3 tries max, editable).");
  }

  function deleteSelection() {
    const edgeId = selEdge();
    const ids = sel();
    if (edgeId) {
      mutate(() => setSpec("edges", (es) => es.filter((e) => e.id !== edgeId)));
      setSelEdge(null);
      return;
    }
    if (ids.size === 0) return;
    mutate(() => {
      // Heal single-through connections the way Fusion does on delete.
      const heals: { from: string; fromPort: OutPort; to: string }[] = [];
      for (const id of ids) {
        const inbound = spec.edges.filter((e) => !e.loop && e.to === id && !ids.has(e.from));
        const outbound = spec.edges.filter((e) => !e.loop && e.from === id && !ids.has(e.to));
        if (inbound.length === 1) {
          for (const out of outbound) {
            heals.push({ from: inbound[0]!.from, fromPort: inbound[0]!.fromPort, to: out.to });
          }
        }
      }
      setSpec("nodes", (ns) => ns.filter((n) => !ids.has(n.id)));
      setSpec("edges", (es) => es.filter((e) => !ids.has(e.from) && !ids.has(e.to)));
      setSpec("groups", (gs) => gs.filter((g) => spec.nodes.some((n) => n.group === g.id)));
      for (const h of heals) {
        if (!spec.edges.some((e) => e.from === h.from && e.to === h.to)) {
          setSpec("edges", (es) => [...es, { id: nodeId(), ...h, loop: false, maxIters: 3 }]);
        }
      }
    });
    setSel(new Set<string>());
  }

  function duplicateSelection() {
    const ids = sel();
    if (ids.size === 0) return;
    const clones: GraphNode[] = [];
    for (const id of ids) {
      const n = nodeById(id);
      if (!n || n.kind === "goal") continue;
      clones.push({ ...structuredClone(unwrap(n)), id: nodeId(), x: n.x + 28, y: n.y + 28, group: null });
    }
    if (clones.length === 0) return;
    mutate(() => setSpec("nodes", (ns) => [...ns, ...clones]));
    setSel(new Set(clones.map((c) => c.id)));
  }

  function patchNode(id: string, patch: Partial<GraphNode>) {
    mutate(() => setSpec("nodes", (n) => n.id === id, patch));
  }

  // ------------------------------------------------------------ interactions

  function onCanvasDown(e: PointerEvent) {
    if (e.button === 2) return;
    setMenu(null);
    setPalette(null);
    setNamePrompt(null);
    const start = toLocal(e.clientX, e.clientY);
    const startCam = cam();
    const isMarquee = e.shiftKey && e.button === 0;
    if (isMarquee) setMarquee({ x0: start.x, y0: start.y, x1: start.x, y1: start.y });
    let moved = false;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);

    const onMove = (ev: PointerEvent) => {
      const cur = toLocal(ev.clientX, ev.clientY);
      if (Math.abs(cur.x - start.x) + Math.abs(cur.y - start.y) > 5) moved = true;
      if (!moved) return;
      if (isMarquee) {
        setMarquee({ x0: start.x, y0: start.y, x1: cur.x, y1: cur.y });
      } else {
        setCam({ k: startCam.k, tx: startCam.tx + cur.x - start.x, ty: startCam.ty + cur.y - start.y });
      }
    };
    const onUp = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      const m = marquee();
      if (isMarquee && m) {
        const a = toWorld(Math.min(m.x0, m.x1), Math.min(m.y0, m.y1));
        const b = toWorld(Math.max(m.x0, m.x1), Math.max(m.y0, m.y1));
        const hit = spec.nodes.filter(
          (n) =>
            !collapsedOf().has(n.id) && n.x + TILE_W > a.x && n.x < b.x && n.y + TILE_H > a.y && n.y < b.y,
        );
        setSel(new Set(hit.map((n) => n.id)));
        setSelEdge(null);
        setMarquee(null);
        return;
      }
      setMarquee(null);
      if (!moved && ev.button === 0) {
        setSel(new Set<string>());
        setSelEdge(null);
      }
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function onCanvasDblClick(e: MouseEvent) {
    if ((e.target as HTMLElement | null)?.closest("[data-node-id],[data-group-id]")) return;
    const s = toLocal(e.clientX, e.clientY);
    openPalette(s.x, s.y);
  }

  /** Shared drag: moves a set of nodes, treating a plain release as a click. */
  function dragNodes(e: PointerEvent, ids: string[], onClick?: () => void) {
    const start = toLocal(e.clientX, e.clientY);
    const origin = new Map<string, { x: number; y: number }>();
    for (const id of ids) {
      const n = nodeById(id);
      if (n) origin.set(id, { x: n.x, y: n.y });
    }
    let moved = false;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);

    const onMove = (ev: PointerEvent) => {
      const cur = toLocal(ev.clientX, ev.clientY);
      if (Math.abs(cur.x - start.x) + Math.abs(cur.y - start.y) > 4) moved = true;
      if (!moved) return;
      const dx = (cur.x - start.x) / cam().k;
      const dy = (cur.y - start.y) / cam().k;
      batch(() => {
        for (const [id, o] of origin) {
          setSpec("nodes", (n) => n.id === id, { x: Math.round(o.x + dx), y: Math.round(o.y + dy) });
        }
      });
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      if (moved) mutate(() => {});
      else onClick?.();
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function onTileDown(e: PointerEvent, node: GraphNode) {
    if (e.button === 2) {
      e.preventDefault();
      const s = toLocal(e.clientX, e.clientY);
      setSel((prev) => (prev.has(node.id) ? prev : new Set([node.id])));
      setMenu({ sx: s.x, sy: s.y, node: node.id });
      return;
    }
    if (e.button !== 0) return;
    e.stopPropagation();
    setMenu(null);
    setPalette(null);
    const additive = e.shiftKey || e.metaKey;
    if (!sel().has(node.id)) {
      setSel((prev) => (additive ? new Set([...prev, node.id]) : new Set([node.id])));
    } else if (additive) {
      setSel((prev) => {
        const next = new Set(prev);
        next.delete(node.id);
        return next;
      });
      return;
    }
    setSelEdge(null);
    dragNodes(e, [...sel()]);
  }

  function onGroupDown(e: PointerEvent, gid: string, members: GraphNode[]) {
    if (e.button === 2) {
      e.preventDefault();
      const s = toLocal(e.clientX, e.clientY);
      setMenu({ sx: s.x, sy: s.y, group: gid });
      return;
    }
    if (e.button !== 0) return;
    e.stopPropagation();
    setMenu(null);
    setPalette(null);
    setSel(new Set<string>());
    setSelEdge(null);
    dragNodes(
      e,
      members.map((m) => m.id),
    );
  }

  function onPortDown(e: PointerEvent, node: GraphNode, port: OutPort) {
    if (e.button !== 0) return;
    e.stopPropagation();
    const p = portOut(node, port);
    setWire({ from: node.id, port, x: p.x, y: p.y });
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);

    const onMove = (ev: PointerEvent) => {
      const s = toLocal(ev.clientX, ev.clientY);
      const w = toWorld(s.x, s.y);
      setWire((prev) => (prev ? { ...prev, x: w.x, y: w.y } : prev));
    };
    const onUp = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      const ghost = wire();
      setWire(null);
      if (!ghost) return;
      const el = document.elementFromPoint(ev.clientX, ev.clientY);
      const targetTile = el?.closest("[data-node-id]") as HTMLElement | null;
      if (targetTile?.dataset.nodeId) {
        connect(ghost.from, ghost.port, targetTile.dataset.nodeId);
        return;
      }
      const targetGroup = el?.closest("[data-group-id]") as HTMLElement | null;
      if (targetGroup?.dataset.groupId) {
        const entry = groupEntry(targetGroup.dataset.groupId);
        if (entry) connect(ghost.from, ghost.port, entry.id);
        return;
      }
      // Dropped on nothing: the Blueprint move — offer the palette, and wire
      // whatever gets picked.
      const s = toLocal(ev.clientX, ev.clientY);
      openPalette(s.x, s.y, { node: ghost.from, port: ghost.port });
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function onKeyDown(e: KeyboardEvent) {
    const el = e.target as HTMLElement | null;
    if (el && (el.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName))) return;
    if (e.key === "Backspace" || e.key === "Delete") {
      e.preventDefault();
      deleteSelection();
    } else if (e.key === "Escape") {
      setPalette(null);
      setMenu(null);
      setWire(null);
      setNamePrompt(null);
    } else if (e.key === "f") {
      fit();
    } else if (e.key === "d" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      duplicateSelection();
    } else if (e.key === "g" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      groupSelection();
    }
  }

  // --------------------------------------------------------------- templates

  async function addFromTemplate(key: string) {
    const t = TEMPLATES.find((x) => x.key === key);
    if (!t) return;
    try {
      const tplSpec = t.build();
      const saved = await api.graphSave({
        projectId: store.activeProject()?.id ?? null,
        name: t.name,
        spec: tplSpec,
      });
      megState.upsert(saved);
      pickGraph(saved.id);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function removeGraph() {
    const d = doc();
    if (!d) return;
    try {
      await api.graphDelete(d.id);
      megState.remove(d.id);
      const next = megState.graphs()[0];
      if (next) pickGraph(next.id);
      else setSelectedId(null);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function clearCache(nodeIdArg?: string) {
    const d = doc();
    if (!d) return;
    try {
      const fresh = await api.graphClearCache(d.id, nodeIdArg);
      megState.upsert(fresh);
      setArts(fresh.artifacts);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  const bound = () => {
    const cid = chatId();
    return cid ? megState.binding(cid) : null;
  };
  async function toggleBind() {
    const cid = chatId();
    const d = doc();
    if (!cid || !d) return;
    try {
      await megState.bind(cid, bound() === d.id ? null : d.id);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  const lastRunCost = createMemo(() => {
    let cost = 0;
    for (const a of Object.values(arts())) cost += a.cost;
    return cost;
  });

  const selectedNode = createMemo(() => {
    const ids = sel();
    if (ids.size !== 1) return null;
    return nodeById([...ids][0]!) ?? null;
  });
  const selectedEdge = createMemo(() => spec.edges.find((e) => e.id === selEdge()) ?? null);

  const rankOf: Record<string, number> = {
    failed: 6,
    waiting: 5,
    running: 4,
    pending: 3,
    cached: 2,
    done: 1,
    skipped: 0,
  };
  function groupStatus(members: GraphNode[]): NodeRunStatus | "idle" {
    let best: NodeRunStatus | "idle" = "idle";
    let bestRank = -1;
    for (const m of members) {
      const s = run.nodes[m.id]?.status;
      if (!s) continue;
      const r = rankOf[s] ?? -1;
      if (r > bestRank) {
        bestRank = r;
        best = s;
      }
    }
    return best;
  }

  // ------------------------------------------------------------------- view

  return (
    <div class="meg">
      <div class="code-head">
        <select
          class="mini-select meg-pick"
          value={selectedId() ?? ""}
          onChange={(e) => pickGraph(e.currentTarget.value)}
          title="Which graph is open"
        >
          <For each={megState.graphs()}>{(g) => <option value={g.id}>{g.name}</option>}</For>
        </select>
        <select
          class="mini-select"
          value=""
          title="Add a graph from a template"
          onChange={(e) => {
            const v = e.currentTarget.value;
            e.currentTarget.value = "";
            if (v) void addFromTemplate(v);
          }}
        >
          <option value="" disabled>
            + template
          </option>
          <For each={TEMPLATES}>{(t) => <option value={t.key}>{t.name}</option>}</For>
        </select>
        <button
          type="button"
          class="btn meg-auto"
          data-size="sm"
          disabled={planning() || busy() || !chatId()}
          title="Let AI design a graph for the goal in the run box (or the last chat message): stages, wiring, loops and per-stage models — you review and adjust it before running"
          onClick={() => void autoPlan()}
        >
          {planning() ? "planning…" : "✦ auto-plan"}
        </button>
        <button
          type="button"
          class="btn"
          data-size="sm"
          data-on={bound() === selectedId() && !!selectedId()}
          title={
            bound() === selectedId()
              ? "Messages in this chat run through this graph — click to unbind"
              : "Route this chat's messages through this graph"
          }
          onClick={() => void toggleBind()}
        >
          <NodeGraph size={12} /> {bound() === selectedId() ? "harness on" : "use for chat"}
        </button>
        <Show when={problems().length > 0}>
          <span class="chip" data-tone="warn" title={problems().join("\n")}>
            {problems().length} issue{problems().length > 1 ? "s" : ""}
          </span>
        </Show>
        <div class="spacer" />
        <Show when={lastRunCost() > 0}>
          <span class="chip tabular" title="Cost of the cached node outputs">
            {formatCost(lastRunCost())}
          </span>
        </Show>
        <button
          type="button"
          class="btn"
          data-size="sm"
          title="Forget every cached node output"
          onClick={() => void clearCache()}
        >
          clear cache
        </button>
        <button
          type="button"
          class="btn"
          data-size="sm"
          title="Delete this graph"
          onClick={() => void removeGraph()}
        >
          <Trash size={12} />
        </button>
        <span class="vdivider" />
        <input
          class="input meg-run-input"
          placeholder="Run with… (empty = last chat message)"
          value={runInput()}
          onInput={(e) => setRunInput(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void runGraph();
          }}
        />
        <Show
          when={!busy()}
          fallback={
            <button
              type="button"
              class="btn"
              data-size="sm"
              data-variant="danger"
              onClick={() => void stopRun()}
            >
              <Stop size={11} /> Stop
            </button>
          }
        >
          <button
            type="button"
            class="btn"
            data-size="sm"
            data-variant="primary"
            disabled={!chatId() || !doc()}
            title="Run the graph in this chat"
            onClick={() => void runGraph()}
          >
            <Play size={11} /> Run
          </button>
        </Show>
      </div>

      <div class="meg-body">
        <div
          class="meg-canvas"
          ref={wrap}
          tabindex="0"
          role="application"
          aria-label="Execution graph canvas"
          style={{
            "background-position": `${cam().tx}px ${cam().ty}px`,
            "background-size": `${24 * cam().k}px ${24 * cam().k}px`,
          }}
          onPointerDown={onCanvasDown}
          onDblClick={onCanvasDblClick}
          onWheel={onWheel}
          onKeyDown={onKeyDown}
        >
          <div
            class="meg-world"
            style={{ transform: `translate(${cam().tx}px, ${cam().ty}px) scale(${cam().k})` }}
          >
            <For each={expandedGroups()}>
              {(g) => (
                <div
                  class="meg-underlay"
                  style={{
                    transform: `translate(${g.x}px, ${g.y}px)`,
                    width: `${g.w}px`,
                    height: `${g.h}px`,
                  }}
                >
                  <button
                    type="button"
                    class="meg-underlay-label"
                    title="Collapse to one tile"
                    onClick={() => setGroupCollapsed(g.id, true)}
                  >
                    ▾ {g.label}
                  </button>
                </div>
              )}
            </For>

            <svg class="meg-wires" aria-hidden="true">
              <For each={spec.edges}>
                {(e) => {
                  const from = () => nodeById(e.from);
                  const to = () => nodeById(e.to);
                  const sameGroup = () => {
                    const a = collapsedOf().get(e.from);
                    const b = collapsedOf().get(e.to);
                    return !!a && !!b && a.group === b.group;
                  };
                  const path = () => {
                    const f = from();
                    const t = to();
                    if (!f || !t || sameGroup()) return "";
                    const p1 = outAnchor(f, e.fromPort);
                    const p2 = inAnchor(t);
                    return wirePath(p1.x, p1.y, p2.x, p2.y);
                  };
                  const active = () => {
                    const f = run.nodes[e.from]?.status;
                    const t = run.nodes[e.to]?.status;
                    return busy() && (t === "running" || (f === "running" && t === "pending"));
                  };
                  const tone = () =>
                    e.loop
                      ? "var(--berry-400)"
                      : e.fromPort === "fail" || e.fromPort === "reject"
                        ? "var(--danger)"
                        : e.fromPort === "pass" || e.fromPort === "approve"
                          ? "var(--ok)"
                          : "var(--text-3)";
                  return (
                    <Show when={path()}>
                      <g
                        class="meg-wire"
                        data-loop={e.loop}
                        data-active={active()}
                        data-selected={selEdge() === e.id}
                      >
                        <path
                          class="meg-wire-hit"
                          d={path()}
                          onPointerDown={(ev) => {
                            ev.stopPropagation();
                            setSelEdge(e.id);
                            setSel(new Set<string>());
                          }}
                        />
                        <path
                          class="meg-wire-line"
                          d={path()}
                          style={{ stroke: selEdge() === e.id ? "var(--select)" : tone() }}
                        />
                        <Show when={e.loop}>
                          {(() => {
                            const mid = () => {
                              const f = from();
                              const t = to();
                              if (!f || !t) return { x: 0, y: 0 };
                              const p1 = outAnchor(f, e.fromPort);
                              const p2 = inAnchor(t);
                              return wireMid(p1.x, p1.y, p2.x, p2.y);
                            };
                            return (
                              <text class="meg-loop-badge" x={mid().x} y={mid().y - 4} text-anchor="middle">
                                {run.loops[e.id]
                                  ? `${run.loops[e.id]!.i}/${run.loops[e.id]!.max}`
                                  : `↺${e.maxIters}`}
                              </text>
                            );
                          })()}
                        </Show>
                      </g>
                    </Show>
                  );
                }}
              </For>
              <Show when={wire()}>
                {(w) => {
                  const src = () => {
                    const n = nodeById(w().from);
                    return n ? portOut(n, w().port) : { x: w().x, y: w().y };
                  };
                  return <path class="meg-wire-ghost" d={wirePath(src().x, src().y, w().x, w().y)} />;
                }}
              </Show>
            </svg>

            <For each={spec.nodes}>
              {(n) => (
                <Show when={!collapsedOf().has(n.id)}>
                  <Tile
                    node={n}
                    live={run.nodes[n.id]}
                    artifact={arts()[n.id]}
                    selected={sel().has(n.id)}
                    busy={busy()}
                    onDown={(e) => onTileDown(e, n)}
                    onPortDown={(e, port) => onPortDown(e, n, port)}
                    onOpen={n.kind === "sketch" ? () => openPadForSketchNode(n) : undefined}
                  />
                </Show>
              )}
            </For>

            <For each={collapsedGroups()}>
              {(g) => (
                // biome-ignore lint/a11y/noStaticElementInteractions: a draggable canvas object; its actions live in the context menu and inspector
                <div
                  class="meg-group"
                  data-group-id={g.id}
                  data-status={groupStatus(g.members)}
                  style={{ transform: `translate(${g.x}px, ${g.y}px)` }}
                  onPointerDown={(e) => onGroupDown(e, g.id, g.members)}
                  onDblClick={(e) => {
                    e.stopPropagation();
                    setGroupCollapsed(g.id, false);
                  }}
                  title={`${g.label} — ${g.members.length} stages inside. Double-click to expand.`}
                >
                  <span class="meg-port-in" />
                  <span class="meg-glyph">▣</span>
                  <span class="meg-node-text">
                    <span class="meg-node-name truncate">{g.label}</span>
                    <span class="meg-node-sub truncate">{g.members.length} stages</span>
                  </span>
                  <span class="meg-node-bar" data-status={groupStatus(g.members)} />
                </div>
              )}
            </For>
          </div>

          <Show when={draft()}>
            {(d) => (
              <div class="meg-draft">
                <div class="meg-draft-text">
                  <strong>✦ Auto-planned.</strong> Check the stages and their models — click any tile to
                  adjust — then run it.
                  <Show when={d().rationale}>
                    <span class="meg-draft-why"> {d().rationale}</span>
                  </Show>
                </div>
                <button
                  type="button"
                  class="btn"
                  data-variant="primary"
                  data-size="sm"
                  disabled={busy()}
                  onClick={() => {
                    setDraft(null);
                    void runGraph();
                  }}
                >
                  <Play size={11} /> Looks right — run
                </button>
                <button
                  type="button"
                  class="btn"
                  data-size="sm"
                  title="Keep the graph for later without running it"
                  onClick={() => setDraft(null)}
                >
                  Keep
                </button>
                <button
                  type="button"
                  class="btn"
                  data-size="sm"
                  data-variant="danger"
                  title="Delete this drafted graph"
                  onClick={() => {
                    setDraft(null);
                    void removeGraph();
                  }}
                >
                  Discard
                </button>
              </div>
            )}
          </Show>

          <Show when={marquee()}>
            {(m) => (
              <div
                class="meg-marquee"
                style={{
                  left: `${Math.min(m().x0, m().x1)}px`,
                  top: `${Math.min(m().y0, m().y1)}px`,
                  width: `${Math.abs(m().x1 - m().x0)}px`,
                  height: `${Math.abs(m().y1 - m().y0)}px`,
                }}
              />
            )}
          </Show>

          <Show when={palette()}>
            {(p) => (
              <AddPalette
                x={p().sx}
                y={p().sy}
                customs={customs()}
                onPick={(kind) => {
                  addNode(kind, p().wx, p().wy, p().connectFrom);
                  setPalette(null);
                }}
                onPickCustom={(def) => {
                  stampCustom(def, p().wx, p().wy);
                  setPalette(null);
                }}
                onDeleteCustom={(name) => persistCustoms(customs().filter((c) => c.name !== name))}
                onClose={() => setPalette(null)}
              />
            )}
          </Show>

          <Show when={menu()}>
            {(m) => (
              <div class="meg-menu" style={{ left: `${m().sx}px`, top: `${m().sy}px` }}>
                <Show when={m().node}>
                  {(nid) => {
                    const n = () => nodeById(nid());
                    return (
                      <>
                        <button
                          type="button"
                          onClick={() => {
                            duplicateSelection();
                            setMenu(null);
                          }}
                        >
                          Duplicate
                        </button>
                        <button
                          type="button"
                          onClick={() => {
                            const node = n();
                            if (node) patchNode(node.id, { enabled: !node.enabled });
                            setMenu(null);
                          }}
                        >
                          {n()?.enabled ? "Bypass" : "Enable"}
                        </button>
                        <button
                          type="button"
                          onClick={() => {
                            const node = n();
                            if (node) patchNode(node.id, { pinned: !node.pinned });
                            setMenu(null);
                          }}
                        >
                          {n()?.pinned ? "Unpin result" : "Pin result"}
                        </button>
                        <Show when={sel().size >= 2}>
                          <button
                            type="button"
                            onClick={() => {
                              groupSelection();
                              setMenu(null);
                            }}
                          >
                            Group ⌘G
                          </button>
                        </Show>
                        <button
                          type="button"
                          onClick={() => {
                            const m2 = menu();
                            setNamePrompt({ sx: m2?.sx ?? 40, sy: m2?.sy ?? 40, ids: [...sel()] });
                            setMenu(null);
                          }}
                        >
                          Save as custom node…
                        </button>
                        <button
                          type="button"
                          data-tone="danger"
                          onClick={() => {
                            deleteSelection();
                            setMenu(null);
                          }}
                        >
                          Delete
                        </button>
                      </>
                    );
                  }}
                </Show>
                <Show when={m().group}>
                  {(gid) => (
                    <>
                      <button
                        type="button"
                        onClick={() => {
                          setGroupCollapsed(
                            gid(),
                            !(spec.groups.find((g) => g.id === gid())?.collapsed ?? true),
                          );
                          setMenu(null);
                        }}
                      >
                        {spec.groups.find((g) => g.id === gid())?.collapsed ? "Expand" : "Collapse"}
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          const ids = spec.nodes.filter((n) => n.group === gid()).map((n) => n.id);
                          const m2 = menu();
                          setNamePrompt({ sx: m2?.sx ?? 40, sy: m2?.sy ?? 40, ids });
                          setMenu(null);
                        }}
                      >
                        Save as custom node…
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          ungroup(gid());
                          setMenu(null);
                        }}
                      >
                        Ungroup
                      </button>
                      <button
                        type="button"
                        data-tone="danger"
                        onClick={() => {
                          setSel(new Set(spec.nodes.filter((n) => n.group === gid()).map((n) => n.id)));
                          deleteSelection();
                          setMenu(null);
                        }}
                      >
                        Delete stages
                      </button>
                    </>
                  )}
                </Show>
              </div>
            )}
          </Show>

          <Show when={namePrompt()}>
            {(p) => (
              <div class="meg-name-prompt" style={{ left: `${p().sx}px`, top: `${p().sy}px` }}>
                <input
                  ref={(el) => setTimeout(() => el.focus(), 30)}
                  class="meg-palette-input"
                  placeholder="Name this custom node…"
                  onKeyDown={(e) => {
                    if (e.key === "Escape") setNamePrompt(null);
                    if (e.key === "Enter") {
                      saveCustom(e.currentTarget.value, p().ids);
                      setNamePrompt(null);
                    }
                  }}
                  onBlur={() => setTimeout(() => setNamePrompt(null), 140)}
                />
              </div>
            )}
          </Show>
        </div>

        <Show when={selectedNode() ?? selectedEdge()}>
          <div class="meg-side">
            <Show when={selectedNode()}>
              {(n) => (
                <NodeInspector
                  node={n()}
                  live={run.nodes[n().id]}
                  artifact={arts()[n().id]}
                  busy={busy()}
                  onPatch={(patch) => patchNode(n().id, patch)}
                  onRefine={(instruction) => void refineNode(n().id, instruction)}
                  onClearCache={() => void clearCache(n().id)}
                  onEditArtifact={(content) => void editArtifact(n().id, content)}
                  onOpenSketch={() => openPadForSketchNode(n())}
                  onClose={() => setSel(new Set<string>())}
                />
              )}
            </Show>
            <Show when={!selectedNode() && selectedEdge()}>
              {(e) => (
                <EdgeInspector
                  edge={e()}
                  fromLabel={nodeById(e().from)?.label ?? "?"}
                  toLabel={nodeById(e().to)?.label ?? "?"}
                  onPatch={(patch) => mutate(() => setSpec("edges", (x) => x.id === e().id, patch))}
                  onDelete={deleteSelection}
                  onClose={() => setSelEdge(null)}
                />
              )}
            </Show>
          </div>
        </Show>
      </div>

      <div class="arch-legend">
        <span>double-click: add node</span>
        <span>drag square port: connect</span>
        <span>backward arrow = loop</span>
        <span>⇧drag: select</span>
        <span>⌘G group</span>
        <span>⌫ delete</span>
        <span>f fit</span>
        <div class="spacer" />
        <For each={["context", "reasoning", "design", "action", "quality"] as const}>
          {(c) => (
            <span class="meg-legend-cat">
              <i style={{ background: CATEGORY_COLOR[c] }} />
              {c}
            </span>
          )}
        </For>
      </div>
    </div>
  );
}

// -------------------------------------------------------------------- pieces

function Tile(props: {
  node: GraphNode;
  live: NodeLive | undefined;
  artifact: NodeArtifact | undefined;
  selected: boolean;
  busy: boolean;
  onDown: (e: PointerEvent) => void;
  onPortDown: (e: PointerEvent, port: OutPort) => void;
  /** Double-click opens the node's own UI (the sketch pad). */
  onOpen?: () => void;
}) {
  const t = () => NODE_TYPE[props.node.kind];
  const color = () => CATEGORY_COLOR[t().category];
  const status = () => props.live?.status ?? "idle";
  const ports = () => outPorts(props.node);
  const subtitle = () => {
    if (props.live?.detail) return props.live.detail;
    const bits: string[] = [];
    if (props.node.model) bits.push(shortModel(props.node.model));
    if (props.node.effort) bits.push(props.node.effort);
    if (bits.length === 0) bits.push(t().free ? "free" : "chat model");
    return bits.join(" · ");
  };
  const costBadge = () => {
    if (status() === "cached") return "⟲ cached";
    const a = props.artifact;
    if (!a || (a.cost === 0 && a.elapsedMs === 0)) return null;
    return `${formatCost(a.cost)} · ${formatDuration(a.elapsedMs)}`;
  };
  const tileTheme = () =>
    props.node.kind === "design" && props.artifact ? parseTheme(props.artifact.content) : null;

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: a draggable canvas object; the inspector's button is the keyboard path to open
    <div
      class="meg-node"
      data-node-id={props.node.id}
      data-status={status()}
      data-selected={props.selected}
      data-disabled={!props.node.enabled}
      data-note={props.node.kind === "note"}
      style={{ transform: `translate(${props.node.x}px, ${props.node.y}px)`, "--cat": color() }}
      onPointerDown={(e) => props.onDown(e)}
      onDblClick={(e) => {
        if (!props.onOpen) return;
        e.stopPropagation();
        props.onOpen();
      }}
      title={`${t().title} — ${t().blurb}${props.onOpen ? "\n\nDouble-click to open the sketch pad." : ""}`}
    >
      <Show when={hasInput(props.node)}>
        <span class="meg-port-in" />
      </Show>
      <span class="meg-glyph" style={{ color: color() }}>
        {t().glyph}
      </span>
      <span class="meg-node-text">
        <span class="meg-node-name truncate">{props.node.label}</span>
        <span class="meg-node-sub truncate">{subtitle()}</span>
      </span>
      <Show when={tileTheme()}>
        {(th) => (
          <span class="meg-tile-swatches">
            <For each={th().colors.slice(0, 5)}>{(c) => <i style={{ background: c.hex }} />}</For>
          </span>
        )}
      </Show>
      <Show when={props.node.pinned}>
        <span class="meg-pin" title="Pinned: always reuses its cached output">
          <Pin size={9} />
        </span>
      </Show>
      <Show when={costBadge()}>{(b) => <span class="meg-cost tabular">{b()}</span>}</Show>
      <For each={ports()}>
        {(port, i) => (
          <span
            class="meg-port-out"
            data-port={port}
            data-two={ports().length > 1}
            data-index={i()}
            title={port === "out" ? "output — drag to connect" : port}
            onPointerDown={(e) => props.onPortDown(e, port)}
          />
        )}
      </For>
      <span class="meg-node-bar" data-status={status()} />
    </div>
  );
}

function AddPalette(props: {
  x: number;
  y: number;
  customs: CustomNodeDef[];
  onPick: (k: NodeKind) => void;
  onPickCustom: (def: CustomNodeDef) => void;
  onDeleteCustom: (name: string) => void;
  onClose: () => void;
}) {
  const [q, setQ] = createSignal("");
  const [cursor, setCursor] = createSignal(0);
  let input!: HTMLInputElement;
  onMount(() => setTimeout(() => input.focus(), 30));

  const builtins = createMemo(() => {
    const query = q().trim().toLowerCase();
    return NODE_TYPES.filter((t) => t.kind !== "note" || query.length > 0).filter(
      (t) =>
        !query ||
        t.title.toLowerCase().includes(query) ||
        t.kind.includes(query) ||
        t.blurb.toLowerCase().includes(query),
    );
  });
  const customHits = createMemo(() => {
    const query = q().trim().toLowerCase();
    return props.customs.filter((c) => !query || c.name.toLowerCase().includes(query));
  });
  const total = () => builtins().length + customHits().length;

  function pickAt(i: number) {
    const b = builtins();
    if (i < b.length) props.onPick(b[i]!.kind);
    else {
      const c = customHits()[i - b.length];
      if (c) props.onPickCustom(c);
    }
  }

  return (
    <div class="meg-palette" style={{ left: `${props.x}px`, top: `${props.y}px` }}>
      <input
        ref={input}
        class="meg-palette-input"
        placeholder="Add node…"
        value={q()}
        onInput={(e) => {
          setQ(e.currentTarget.value);
          setCursor(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") props.onClose();
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setCursor((c) => Math.min(c + 1, total() - 1));
          }
          if (e.key === "ArrowUp") {
            e.preventDefault();
            setCursor((c) => Math.max(c - 1, 0));
          }
          if (e.key === "Enter" && total() > 0) pickAt(cursor());
        }}
        onBlur={() => setTimeout(props.onClose, 140)}
      />
      <div class="meg-palette-list">
        <For each={builtins()}>
          {(t, i) => (
            <button
              type="button"
              class="meg-palette-row"
              data-active={i() === cursor()}
              onPointerDown={(e) => {
                e.preventDefault();
                props.onPick(t.kind);
              }}
              onPointerEnter={() => setCursor(i())}
            >
              <i style={{ background: CATEGORY_COLOR[t.category] }} />
              <span class="meg-palette-name">
                {t.glyph} {t.title}
              </span>
              <span class="meg-palette-blurb truncate">{t.blurb}</span>
            </button>
          )}
        </For>
        <Show when={customHits().length > 0}>
          <div class="meg-palette-divider">custom nodes</div>
          <For each={customHits()}>
            {(c, i) => (
              <div
                class="meg-palette-row"
                data-active={builtins().length + i() === cursor()}
                onPointerEnter={() => setCursor(builtins().length + i())}
              >
                <button
                  type="button"
                  class="meg-palette-main"
                  onPointerDown={(e) => {
                    e.preventDefault();
                    props.onPickCustom(c);
                  }}
                >
                  <i style={{ background: "var(--text-2)" }} />
                  <span class="meg-palette-name">▣ {c.name}</span>
                  <span class="meg-palette-blurb truncate">{c.nodes.length} stages</span>
                </button>
                <button
                  type="button"
                  class="meg-palette-del"
                  title="Remove from library"
                  onPointerDown={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    props.onDeleteCustom(c.name);
                  }}
                >
                  ×
                </button>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}

const EFFORTS = ["off", "low", "medium", "high"];
const TOOL_LEVELS: [string, string][] = [
  ["none", "no tools"],
  ["read", "read code"],
  ["web", "web"],
  ["edit", "read + edit"],
  ["full", "edit + run"],
];
const HISTORY_CHOICES: [number, string][] = [
  [0, "off"],
  [4, "last 4 messages"],
  [12, "last 12 messages"],
  [30, "last 30 messages"],
];

/** The design node's palette and wireframe — rendered live AND hand-editable:
 * click a swatch to pick a new colour, drag an area to move it, drag its
 * corner handle to resize, double-click to rename. Every edit rewrites the
 * node's design JSON and pins the node, so your version survives re-runs. */
function DesignEditor(props: {
  node: GraphNode;
  artifact: NodeArtifact | undefined;
  liveText: string | undefined;
  streaming: boolean;
  onCommit: (content: string) => void;
}) {
  const content = () => props.liveText || props.artifact?.content || "";
  const theme = createMemo(() => (content() ? parseTheme(content()) : null));
  const wireframe = createMemo(() => (content() ? parseWireframe(content()) : null));
  const canEdit = () => !props.streaming && !!props.artifact?.content;

  const commit = (mutate: (d: DesignJson) => void) => {
    const src = props.artifact?.content;
    if (!src) return;
    const next = updateDesignJson(src, mutate);
    if (!next) {
      toast("warn", "This design has no structured JSON block to edit — regenerate the node once.");
      return;
    }
    props.onCommit(next);
  };

  const themeColor = (re: RegExp) => theme()?.colors.find((c) => re.test(c.role))?.hex;
  const alpha = (hex: string, aa: string) => (hex.length === 7 ? hex + aa : hex);

  const setColor = (c: { role: string; hex: string }, hex: string) =>
    commit((d) => {
      const list = d.theme?.colors ?? [];
      const target =
        list.find((x) => (x.role ?? "") === c.role) ??
        list.find((x) =>
          String(x.hex ?? x.color ?? "")
            .toLowerCase()
            .includes(c.hex.slice(1).toLowerCase()),
        );
      if (target) target.hex = hex;
    });

  // Drag state: a live preview of the area being moved or resized; the JSON
  // is rewritten once, on release.
  const [drag, setDrag] = createSignal<{ label: string; x: number; y: number; w: number; h: number } | null>(
    null,
  );
  const [renaming, setRenaming] = createSignal<string | null>(null);
  let wfEl: HTMLDivElement | undefined;

  function startDrag(e: PointerEvent, a: WireArea, cols: number, rows: number, resize: boolean) {
    if (!canEdit() || !wfEl || e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const rect = wfEl.getBoundingClientRect();
    const cellW = rect.width / cols;
    const cellH = rect.height / rows;
    const sx = e.clientX;
    const sy = e.clientY;
    const orig = { x: a.x, y: a.y, w: a.w, h: a.h };
    setDrag({ label: a.label, ...orig });
    const move = (ev: PointerEvent) => {
      const dx = Math.round((ev.clientX - sx) / Math.max(1, cellW));
      const dy = Math.round((ev.clientY - sy) / Math.max(1, cellH));
      setDrag(
        resize
          ? {
              label: a.label,
              x: orig.x,
              y: orig.y,
              w: Math.max(1, Math.min(cols - orig.x, orig.w + dx)),
              h: Math.max(1, Math.min(rows - orig.y, orig.h + dy)),
            }
          : {
              label: a.label,
              x: Math.max(0, Math.min(cols - orig.w, orig.x + dx)),
              y: Math.max(0, Math.min(rows - orig.h, orig.y + dy)),
              w: orig.w,
              h: orig.h,
            },
      );
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const d = drag();
      setDrag(null);
      if (!d || (d.x === orig.x && d.y === orig.y && d.w === orig.w && d.h === orig.h)) return;
      commit((specJson) => {
        const area = (specJson.wireframe?.areas ?? []).find(
          (m) => m.label === a.label && Number(m.x ?? 0) === orig.x && Number(m.y ?? 0) === orig.y,
        );
        if (area) {
          area.x = d.x;
          area.y = d.y;
          area.w = d.w;
          area.h = d.h;
        }
      });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  const rename = (a: WireArea, label: string) => {
    setRenaming(null);
    const next = label.trim();
    if (!next || next === a.label) return;
    commit((d) => {
      const area = (d.wireframe?.areas ?? []).find((m) => m.label === a.label);
      if (area) area.label = next;
    });
  };

  return (
    <>
      <Show when={theme()}>
        {(th) => (
          <div class="meg-theme">
            <span class="meg-field-label">
              {th().name} · {th().colors.length} colours
              <Show when={canEdit()}>
                <span class="muted"> — click a swatch to edit</span>
              </Show>
            </span>
            <div class="meg-swatches">
              <For each={th().colors}>
                {(c) => (
                  <label class="meg-swatch" style={{ background: c.hex }} title={`${c.role} ${c.hex}`}>
                    <input
                      type="color"
                      class="meg-swatch-input"
                      value={c.hex.length === 7 ? c.hex : "#888888"}
                      disabled={!canEdit()}
                      onChange={(e) => setColor(c, e.currentTarget.value)}
                    />
                  </label>
                )}
              </For>
            </div>
          </div>
        )}
      </Show>

      <Show when={wireframe()}>
        {(wf) => {
          const bg = themeColor(/bg|background|base|canvas/i);
          const surface = themeColor(/surface|card|panel|container|secondary/i);
          const accent = themeColor(/accent|primary|cta|highlight|brand/i);
          const ink = themeColor(/text|foreground|ink/i);
          return (
            <div class="meg-theme">
              <span class="meg-field-label">
                wireframe — {wf().name}
                <Show when={canEdit()}>
                  <span class="muted"> — drag to move, corner to resize, double-click to rename</span>
                </Show>
              </span>
              <div
                ref={wfEl}
                class="meg-wireframe"
                data-editable={canEdit()}
                style={{
                  "aspect-ratio": `${wf().cols} / ${wf().rows}`,
                  ...(bg ? { background: bg } : {}),
                }}
              >
                <For each={wf().areas}>
                  {(a) => {
                    const d = () => (drag()?.label === a.label ? drag()! : a);
                    const tint = a.role === "cta" && accent ? accent : surface;
                    return (
                      // biome-ignore lint/a11y/noStaticElementInteractions: drag-editing surface; renaming has a real input, and edits are also possible via the JSON itself
                      <div
                        class="meg-wf-area"
                        data-role={a.role ?? "content"}
                        data-dragging={drag()?.label === a.label}
                        style={{
                          left: `${(d().x / wf().cols) * 100}%`,
                          top: `${(d().y / wf().rows) * 100}%`,
                          width: `${(Math.min(d().w, wf().cols - d().x) / wf().cols) * 100}%`,
                          height: `${(Math.min(d().h, wf().rows - d().y) / wf().rows) * 100}%`,
                          ...(tint
                            ? {
                                background: alpha(tint, a.role === "cta" ? "44" : "2e"),
                                "border-color": tint,
                              }
                            : {}),
                          ...(ink ? { color: ink } : {}),
                        }}
                        title={`${a.label}${a.role ? ` (${a.role})` : ""}`}
                        onPointerDown={(e) => startDrag(e, a, wf().cols, wf().rows, false)}
                        onDblClick={() => canEdit() && setRenaming(a.label)}
                      >
                        <Show
                          when={renaming() === a.label}
                          fallback={<span class="truncate">{a.label}</span>}
                        >
                          <input
                            class="meg-wf-rename"
                            value={a.label}
                            ref={(el) => queueMicrotask(() => el.select())}
                            onPointerDown={(e) => e.stopPropagation()}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") rename(a, e.currentTarget.value);
                              if (e.key === "Escape") setRenaming(null);
                            }}
                            onBlur={(e) => rename(a, e.currentTarget.value)}
                          />
                        </Show>
                        <Show when={canEdit()}>
                          <span
                            class="meg-wf-handle"
                            onPointerDown={(e) => startDrag(e, a, wf().cols, wf().rows, true)}
                          />
                        </Show>
                      </div>
                    );
                  }}
                </For>
              </div>
            </div>
          );
        }}
      </Show>
    </>
  );
}

function NodeInspector(props: {
  node: GraphNode;
  live: NodeLive | undefined;
  artifact: NodeArtifact | undefined;
  busy: boolean;
  onPatch: (p: Partial<GraphNode>) => void;
  onRefine: (instruction: string) => void;
  onClearCache: () => void;
  onEditArtifact: (content: string) => void;
  onOpenSketch: () => void;
  onClose: () => void;
}) {
  const t = () => NODE_TYPE[props.node.kind];
  const verdict = createMemo<Verdict | null>(() => {
    const raw = props.artifact?.verdict;
    if (!raw) return null;
    try {
      return JSON.parse(raw) as Verdict;
    } catch {
      return null;
    }
  });
  const output = () => props.live?.text || props.artifact?.content || "";
  const streaming = () => props.live?.status === "running";

  const patchConfig = (patch: Partial<GraphNode["config"]>) =>
    props.onPatch({ config: { ...props.node.config, ...patch } });

  return (
    <>
      <div class="meg-side-head">
        <span class="meg-glyph" style={{ color: CATEGORY_COLOR[t().category] }}>
          {t().glyph}
        </span>
        <input
          class="meg-side-name"
          value={props.node.label}
          onChange={(e) => props.onPatch({ label: e.currentTarget.value.trim() || t().title })}
        />
        <button type="button" class="btn" data-size="icon-sm" title="Close" onClick={props.onClose}>
          <Close size={11} />
        </button>
      </div>
      <div class="meg-side-scroll">
        <p class="meg-side-blurb">{t().blurb}</p>

        <Show when={props.node.kind === "design"}>
          <DesignEditor
            node={props.node}
            artifact={props.artifact}
            liveText={props.live?.text}
            streaming={streaming()}
            onCommit={props.onEditArtifact}
          />
        </Show>

        <Show when={props.node.kind === "sketch"}>
          <div class="meg-field">
            <button
              type="button"
              class="btn"
              data-variant="primary"
              data-size="sm"
              onClick={props.onOpenSketch}
            >
              ✏ Open the sketch pad
            </button>
            <p class="meg-side-blurb">
              {props.artifact?.content
                ? "A drawing is stored on this node — runs use it without pausing. Open the pad to redraw it, then Save."
                : "Draw the wireframe — or paste a screenshot and annotate over it — then Save to store it on this node. A run that arrives with nothing stored pauses and opens the pad for you. Double-clicking the tile opens it too."}
            </p>
          </div>
        </Show>

        <div class="meg-field-row">
          <label class="meg-toggle" title="A bypassed node passes its inputs straight through">
            <input
              type="checkbox"
              checked={props.node.enabled}
              onChange={(e) => props.onPatch({ enabled: e.currentTarget.checked })}
            />
            enabled
          </label>
          <label
            class="meg-toggle"
            title="Pinned nodes always serve their cached output and never spend tokens — reuse research or a theme across runs"
          >
            <input
              type="checkbox"
              checked={props.node.pinned}
              onChange={(e) => props.onPatch({ pinned: e.currentTarget.checked })}
            />
            pin result
          </label>
        </div>

        <Show when={!t().free}>
          <div class="meg-field">
            <span class="meg-field-label">model{props.node.model ? "" : " — following the chat"}</span>
            <div class="meg-field-row">
              <ModelPicker
                value={props.node.model ?? store.activeChat()?.model ?? store.settings()?.defaultModel ?? ""}
                onChange={(id) => props.onPatch({ model: id })}
                drop="down"
              />
              <Show when={props.node.model}>
                <button
                  type="button"
                  class="btn"
                  data-size="sm"
                  title="Follow the chat's model instead"
                  onClick={() => props.onPatch({ model: null })}
                >
                  chat's
                </button>
              </Show>
            </div>
          </div>

          <div class="meg-field-row">
            <div class="meg-field">
              <span class="meg-field-label">think</span>
              <select
                class="mini-select"
                value={props.node.effort ?? ""}
                onChange={(e) => props.onPatch({ effort: e.currentTarget.value || null })}
              >
                <option value="">inherit</option>
                <For each={EFFORTS}>{(v) => <option value={v}>{v}</option>}</For>
              </select>
            </div>
            <div class="meg-field">
              <span class="meg-field-label" title="When does this node actually execute again?">
                re-run
              </span>
              <select
                class="mini-select"
                value={props.node.config.rerun ?? "auto"}
                onChange={(e) => {
                  const v = e.currentTarget.value;
                  patchConfig({ rerun: v === "auto" ? undefined : (v as "always" | "once") });
                }}
              >
                <option value="auto">when inputs change</option>
                <option value="always">every run</option>
                <option value="once">once per run — loops reuse it</option>
              </select>
            </div>
          </div>

          <div class="meg-field-row">
            <div class="meg-field">
              <span class="meg-field-label" title="Give this node the conversation so far">
                history
              </span>
              <select
                class="mini-select"
                value={String(props.node.config.history ?? 0)}
                onChange={(e) => {
                  const n = Number(e.currentTarget.value);
                  patchConfig({ history: n > 0 ? n : undefined });
                }}
              >
                <For each={HISTORY_CHOICES}>{([v, label]) => <option value={String(v)}>{label}</option>}</For>
              </select>
            </div>
            <Show when={t().agentic}>
              <label
                class="meg-toggle"
                title="Let this stage search and read past conversations (memory tools)"
                style={{ "margin-top": "14px" }}
              >
                <input
                  type="checkbox"
                  checked={props.node.config.memory ?? false}
                  onChange={(e) => patchConfig({ memory: e.currentTarget.checked || undefined })}
                />
                memory
              </label>
            </Show>
          </div>
        </Show>

        <Show when={t().agentic}>
          <div class="meg-field">
            <span class="meg-field-label">tools</span>
            <select
              class="mini-select"
              value={props.node.config.tools ?? t().defaults.tools ?? "none"}
              onChange={(e) => patchConfig({ tools: e.currentTarget.value as never })}
            >
              <For each={TOOL_LEVELS}>{([v, label]) => <option value={v}>{label}</option>}</For>
            </select>
          </div>
        </Show>

        <Show when={t().verdict && props.node.kind !== "gate"}>
          <div class="meg-field">
            <span class="meg-field-label">when loop budget runs out</span>
            <select
              class="mini-select"
              value={props.node.config.onExhaust ?? "continue"}
              onChange={(e) => patchConfig({ onExhaust: e.currentTarget.value as never })}
            >
              <option value="continue">continue anyway</option>
              <option value="fail">fail the run</option>
            </select>
          </div>
        </Show>

        <Show
          when={
            props.node.kind !== "merge" && props.node.kind !== "archmap" && props.node.kind !== "mapupdate"
          }
        >
          <div class="meg-field">
            <div class="meg-field-row">
              <span class="meg-field-label">
                {props.node.kind === "sketch"
                  ? "note shown to you when the pad opens — no AI reads this"
                  : "instruction"}{" "}
                <Show when={!t().agentic && props.node.kind !== "sketch" && props.node.prompt.trim() === ""}>
                  <em>(empty = pass-through, free)</em>
                </Show>
              </span>
              <div class="spacer" />
              <Show when={t().prompt && props.node.prompt.trim() !== t().prompt.trim()}>
                <button
                  type="button"
                  class="btn"
                  data-size="sm"
                  title="Restore this node type's standard instruction — includes its newest capabilities (the design node's theme + wireframe output, for example)"
                  onClick={() => props.onPatch({ prompt: t().prompt })}
                >
                  reset
                </button>
              </Show>
            </div>
            <textarea
              class="meg-prompt"
              rows={7}
              value={props.node.prompt}
              spellcheck={false}
              onChange={(e) => props.onPatch({ prompt: e.currentTarget.value })}
            />
          </div>
        </Show>

        <Show when={verdict()}>
          {(v) => (
            <div class="meg-verdict" data-pass={v().pass}>
              <strong>{v().pass ? "✓ pass" : "✗ fail"}</strong>
              <Show when={v().score !== null}> · score {Math.round((v().score ?? 0) * 100) / 100}</Show>
              <Show when={v().summary}> — {v().summary}</Show>
              <For each={v().issues}>{(i) => <div class="meg-verdict-issue">• {i}</div>}</For>
            </div>
          )}
        </Show>

        <Show when={output() && !streaming() && !t().free && props.node.kind !== "mapupdate"}>
          <div class="meg-field">
            <span class="meg-field-label">refine — command this output to change</span>
            <input
              class="input meg-refine"
              placeholder="e.g. warmer palette · tighter plan · stricter checks  ⏎"
              disabled={props.busy}
              onKeyDown={(e) => {
                if (e.key === "Enter" && e.currentTarget.value.trim()) {
                  props.onRefine(e.currentTarget.value.trim());
                  e.currentTarget.value = "";
                }
              }}
            />
          </div>
        </Show>

        <Show when={output()}>
          <div class="meg-field">
            <div class="meg-field-row">
              <span class="meg-field-label">{streaming() ? "output — live" : "last output"}</span>
              <div class="spacer" />
              <Show when={props.artifact}>
                {(a) => (
                  <span class="meg-out-meta tabular">
                    {formatTokens(a().tokensIn + a().tokensOut)} tok · {formatCost(a().cost)}
                  </span>
                )}
              </Show>
              <Show when={props.artifact && !streaming()}>
                <button
                  type="button"
                  class="btn"
                  data-size="sm"
                  title="Forget this cached output"
                  onClick={props.onClearCache}
                >
                  clear
                </button>
              </Show>
            </div>
            <div class="meg-output selectable">
              <Markdown text={output()} streaming={streaming()} />
            </div>
          </div>
        </Show>
      </div>
    </>
  );
}

function EdgeInspector(props: {
  edge: { id: string; loop: boolean; maxIters: number; fromPort: OutPort };
  fromLabel: string;
  toLabel: string;
  onPatch: (p: { maxIters?: number }) => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  return (
    <>
      <div class="meg-side-head">
        <span class="meg-glyph">{props.edge.loop ? "↺" : "→"}</span>
        <span class="meg-side-name meg-side-name-static truncate">
          {props.fromLabel} → {props.toLabel}
        </span>
        <button type="button" class="btn" data-size="icon-sm" title="Close" onClick={props.onClose}>
          <Close size={11} />
        </button>
      </div>
      <div class="meg-side-scroll">
        <p class="meg-side-blurb">
          {props.edge.loop
            ? "A loop arrow: when the verdict fails, the flow goes back and tries again, up to the budget below."
            : props.edge.fromPort === "out"
              ? "Carries this node's output downstream as context."
              : `Fires only on ${props.edge.fromPort}.`}
        </p>
        <Show when={props.edge.loop}>
          <div class="meg-field">
            <span class="meg-field-label">max tries</span>
            <input
              class="input"
              type="number"
              min="1"
              max="9"
              value={props.edge.maxIters}
              onChange={(e) =>
                props.onPatch({ maxIters: Math.max(1, Math.min(9, Number(e.currentTarget.value) || 3)) })
              }
            />
          </div>
        </Show>
        <button type="button" class="btn" data-size="sm" data-variant="danger" onClick={props.onDelete}>
          <Trash size={11} /> Remove arrow
        </button>
      </div>
    </>
  );
}
