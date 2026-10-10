// Force layout and similarity scoring for the architecture graph.
//
// No graph library: the layout is ~100 lines of physics, which keeps the
// bundle lean and every visual decision in our hands. Repulsion goes through
// a spatial hash so a 1500-file repo still settles in well under a second,
// and the simulation runs in chunks so a frame is never blocked.

import type { ArchDoc, ArchFn } from "./types";

export interface GNode {
  id: string;
  label: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
  r: number;
  /** Index into the group (top-level directory) list. */
  group: number;
  quality: number;
  degree: number;
  /** Number of distinct edges touching this node — normalises spring force. */
  links: number;
  /** 0 = most connected. Drives the zoom-scaled label budget. */
  labelRank: number;
  /** Width of the label as last drawn, in world units. 0 = not on screen.
   * Lets the hit test treat the caption as part of the node. */
  labelW: number;
  /** Effective radius for layout: half the expected tile footprint, so the
   * physics spaces named tiles like tiles, not like dots. */
  er: number;
  /** Architectural kind (entry | orchestrator | ui | …), when classified. */
  kind: string | null;
  /** Worst function complexity in the file — the optimisation heat signal. */
  maxCx: number;
  fnCount: number;
  hub: boolean;
  /** Channels this file exposes (bound routes, subscribed topics) — the
   * "this is a service surface" badge. */
  serves: string[];
  pinned: boolean;
}

export interface GEdge {
  a: number;
  b: number;
  weight: number;
  width: number;
  /** Route/event labels carried by this pair — drawn as a channel link. */
  via: string[];
}

export interface Graph {
  nodes: GNode[];
  edges: GEdge[];
  groups: string[];
  /** One anchor point per group; the layout gravitates each directory's files
   * to its own island, which is what makes the map read as clusters. */
  anchors: { x: number; y: number }[];
  byId: Map<string, number>;
  /** Edges beyond the render cap, dropped weakest-first. 0 for normal repos. */
  droppedEdges: number;
}

/** Beyond this the picture is hairball, not information — keep the strongest. */
const MAX_EDGES = 6000;

/** Deterministic PRNG so the same repo always lands in the same shape. */
function lcg(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0xffffffff;
  };
}

function hashCode(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/** Grouping key: the first two path segments. Exported so the legend can
 * reproduce the exact hue assignment the graph uses. */
export function topDir(id: string): string {
  const i = id.indexOf("/");
  if (i === -1) return "(root)";
  const j = id.indexOf("/", i + 1);
  return j === -1 ? id.slice(0, i) : id.slice(0, j);
}

/** Grouping key inside a folder scope: the next path level under `base`, so
 * a scoped map re-clusters by the scope's own subfolders. */
export function topDirUnder(id: string, base: string | null): string {
  if (!base) return topDir(id);
  const rest = id.startsWith(`${base}/`) ? id.slice(base.length + 1) : id;
  const i = rest.indexOf("/");
  return i === -1 ? "(files)" : `${base}/${rest.slice(0, i)}`;
}

/** Folds directed doc edges into undirected pairs with weights, via labels
 * and per-file degree — the graph's raw material. */
function foldEdges(doc: ArchDoc): {
  pair: Map<string, number>;
  pairVia: Map<string, string[]>;
  degree: Map<string, number>;
} {
  const pair = new Map<string, number>();
  const pairVia = new Map<string, string[]>();
  for (const e of doc.edges) {
    const k = e.from < e.to ? `${e.from}\u0000${e.to}` : `${e.to}\u0000${e.from}`;
    pair.set(k, (pair.get(k) ?? 0) + e.weight);
    if ((e.via ?? []).length > 0) {
      const list = pairVia.get(k) ?? [];
      for (const v of e.via) if (!list.includes(v)) list.push(v);
      pairVia.set(k, list);
    }
  }
  const degree = new Map<string, number>();
  for (const [k, w] of pair) {
    const [a, b] = k.split("\u0000") as [string, string];
    degree.set(a, (degree.get(a) ?? 0) + w);
    degree.set(b, (degree.get(b) ?? 0) + w);
  }
  return { pair, pairVia, degree };
}

export function buildGraph(
  doc: ArchDoc,
  width: number,
  height: number,
  groupKey: (id: string) => string = topDir,
): Graph {
  const byId = new Map<string, number>();
  const groupNames: string[] = [];
  const groupOf = new Map<string, number>();

  // Undirected edge weights: coupling is what the eye reads, not direction.
  const { pair, pairVia, degree } = foldEdges(doc);

  // Duplicate basenames (every project has three structure.ts) get their
  // parent directory in the caption, so the map never shows two identical
  // labels meaning different files.
  const baseCount = new Map<string, number>();
  for (const f of doc.files) {
    const base = f.id.split("/").pop() ?? f.id;
    baseCount.set(base, (baseCount.get(base) ?? 0) + 1);
  }
  const captionOf = (id: string): string => {
    const parts = id.split("/");
    const base = parts.pop() ?? id;
    const parent = parts.pop();
    return (baseCount.get(base) ?? 0) > 1 && parent ? `${parent}/${base}` : base;
  };

  const nodes: GNode[] = doc.files.map((f, i) => {
    byId.set(f.id, i);
    const dir = groupKey(f.id);
    let g = groupOf.get(dir);
    if (g === undefined) {
      g = groupNames.length;
      groupNames.push(dir);
      groupOf.set(dir, g);
    }
    const deg = degree.get(f.id) ?? 0;
    return {
      id: f.id,
      label: captionOf(f.id),
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      r: Math.min(19, 5.5 + Math.sqrt(deg) * 1.5),
      group: g,
      quality: f.quality,
      // The scan persists these; deriving here is only the fallback for maps
      // written before the fields existed.
      degree: (f.degree ?? 0) > 0 ? f.degree : deg,
      links: 0,
      labelRank: 0,
      labelW: 0,
      er: 0,
      kind: f.kind,
      maxCx: (f.maxCx ?? 0) > 0 ? f.maxCx : f.functions.reduce((m, fn) => Math.max(m, fn.complexity), 0),
      fnCount: f.functions.length,
      hub: f.hub ?? false,
      serves: (f.provides ?? []).slice(0, 4),
      pinned: false,
    };
  });

  // Hubs come from the document; the cut is only computed for old maps.
  const storedHubs = doc.files.some((f) => f.hub);
  const sorted = [...nodes].sort((a, b) => b.degree - a.degree);
  const hubCut = sorted[Math.floor(nodes.length * 0.08)]?.degree ?? Infinity;
  sorted.forEach((n, rank) => {
    n.labelRank = rank;
    if (!storedHubs) n.hub = n.degree > 0 && n.degree >= hubCut;
    // ~5.7px per mono character at tile font size, plus padding. Nodes likely
    // to render as tiles claim tile-sized space; deep-background dots do not.
    const approxTileW = n.label.length * 5.7 + 16;
    n.er = Math.max(n.r, approxTileW * (rank < 60 ? 0.48 : 0.24), 11);
  });

  // Group anchors on a ring that grows with the repo, so clusters have room
  // to be islands instead of one heap. Seeding is deterministic: the same
  // repo always lands in the same shape.
  const cx = width / 2;
  const cy = height / 2;
  const ringR = Math.max(Math.min(width, height) * 0.34, Math.sqrt(nodes.length) * 48);
  const anchors = groupNames.map((_, g) => {
    const angle = (g / Math.max(1, groupNames.length)) * Math.PI * 2 - Math.PI / 2;
    return { x: cx + Math.cos(angle) * ringR, y: cy + Math.sin(angle) * ringR };
  });
  for (const n of nodes) {
    const rand = lcg(hashCode(n.id));
    const a = anchors[n.group] ?? { x: cx, y: cy };
    n.x = a.x + (rand() - 0.5) * 160;
    n.y = a.y + (rand() - 0.5) * 160;
  }

  let edges: GEdge[] = [];
  for (const [k, w] of pair) {
    const [fa, fb] = k.split("\u0000") as [string, string];
    const a = byId.get(fa);
    const b = byId.get(fb);
    if (a === undefined || b === undefined) continue;
    // Width lives in world units and scales with zoom, so a heavy edge reads
    // as "thicker than its neighbours", never as a screen-filling chord.
    edges.push({
      a,
      b,
      weight: w,
      width: Math.min(3.2, 0.6 + Math.log2(1 + w) * 0.75),
      via: pairVia.get(k) ?? [],
    });
  }
  let droppedEdges = 0;
  if (edges.length > MAX_EDGES) {
    edges.sort((x, y) => y.weight - x.weight);
    droppedEdges = edges.length - MAX_EDGES;
    edges = edges.slice(0, MAX_EDGES);
  }
  for (const e of edges) {
    nodes[e.a]!.links++;
    nodes[e.b]!.links++;
  }

  return { nodes, edges, groups: groupNames, anchors, byId, droppedEdges };
}

/**
 * Drill-down view: one file opened up into its functions. Nodes are the
 * functions (sized by how much they are called in-file, heat from
 * complexity), edges are intra-file calls, and groups come from the model's
 * tags so math/routing/io land as their own little islands.
 */
export function buildFnGraph(doc: ArchDoc, fileId: string, width: number, height: number): Graph {
  const file = doc.files.find((f) => f.id === fileId);
  const fns = file?.functions ?? [];
  const byId = new Map<string, number>();
  const groupNames: string[] = [];
  const groupOf = new Map<string, number>();
  const indexOfFn = new Map<string, number>();
  fns.forEach((fn, i) => {
    indexOfFn.set(fn.name, i);
  });

  // Intra-file call edges first, so degree is known before sizing nodes.
  const pair = new Map<string, number>();
  fns.forEach((fn, i) => {
    for (const call of fn.calls) {
      const j = indexOfFn.get(call);
      if (j === undefined || j === i) continue;
      const k = i < j ? `${i}:${j}` : `${j}:${i}`;
      pair.set(k, (pair.get(k) ?? 0) + 1);
    }
  });
  const degree = new Map<number, number>();
  for (const [k, w] of pair) {
    const [a, b] = k.split(":").map(Number) as [number, number];
    degree.set(a, (degree.get(a) ?? 0) + w);
    degree.set(b, (degree.get(b) ?? 0) + w);
  }

  const nodes: GNode[] = fns.map((fn, i) => {
    byId.set(fn.name, i);
    const tag = fn.tag ?? (fn.exported ? "exported" : "internal");
    let g = groupOf.get(tag);
    if (g === undefined) {
      g = groupNames.length;
      groupNames.push(tag);
      groupOf.set(tag, g);
    }
    const deg = degree.get(i) ?? 0;
    return {
      id: fn.name,
      label: fn.name,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      r: Math.min(19, 6 + Math.sqrt(deg) * 1.6),
      group: g,
      quality: fn.quality,
      degree: deg,
      links: 0,
      labelRank: 0,
      labelW: 0,
      er: 0,
      kind: null,
      maxCx: fn.complexity,
      fnCount: 0,
      hub: fn.core,
      serves: [],
      pinned: false,
    };
  });

  const sorted = [...nodes].sort((a, b) => b.degree - a.degree);
  sorted.forEach((n, rank) => {
    n.labelRank = rank;
    n.er = Math.max(n.r, (n.label.length * 5.7 + 16) * 0.48, 11);
  });

  const cx = width / 2;
  const cy = height / 2;
  const ringR = Math.max(Math.min(width, height) * 0.28, Math.sqrt(Math.max(1, nodes.length)) * 36);
  const anchors = groupNames.map((_, g) => {
    const angle = (g / Math.max(1, groupNames.length)) * Math.PI * 2 - Math.PI / 2;
    return { x: cx + Math.cos(angle) * ringR, y: cy + Math.sin(angle) * ringR };
  });
  for (const n of nodes) {
    const rand = lcg(hashCode(`${fileId}::${n.id}`));
    const a = anchors[n.group] ?? { x: cx, y: cy };
    n.x = a.x + (rand() - 0.5) * 140;
    n.y = a.y + (rand() - 0.5) * 140;
  }

  const edges: GEdge[] = [];
  for (const [k, w] of pair) {
    const [a, b] = k.split(":").map(Number) as [number, number];
    edges.push({ a, b, weight: w, width: Math.min(3.2, 0.7 + Math.log2(1 + w) * 0.8), via: [] });
    nodes[a]!.links++;
    nodes[b]!.links++;
  }

  return { nodes, edges, groups: groupNames, anchors, byId, droppedEdges: 0 };
}

/**
 * One simulation tick. `alpha` decays from 1 → 0 over the run; forces scale
 * with it so the layout settles instead of jittering forever.
 */
export function stepSim(graph: Graph, alpha: number, width: number, height: number) {
  applySprings(graph, alpha);
  applyRepulsion(graph.nodes, alpha);
  applyClusterSeparation(graph, alpha);
  integrate(graph, alpha, width / 2, height / 2);
}

/**
 * Long-range order the local repulsion cannot provide: whole clusters push
 * apart when their expected footprints overlap. This is what turns a 1000-file
 * heap into an archipelago — each directory a clearly separated island.
 */
function clusterCentroids(graph: Graph, gs: number) {
  const cx = new Float64Array(gs);
  const cy = new Float64Array(gs);
  const cnt = new Float64Array(gs);
  for (const n of graph.nodes) {
    cx[n.group]! += n.x;
    cy[n.group]! += n.y;
    cnt[n.group]! += 1;
  }
  for (let g = 0; g < gs; g++) {
    if (cnt[g]! > 0) {
      cx[g]! /= cnt[g]!;
      cy[g]! /= cnt[g]!;
    }
  }
  return { cx, cy, cnt };
}

function applyClusterSeparation(graph: Graph, alpha: number) {
  const gs = graph.groups.length;
  // Tiny graphs (a drilled file's functions) fit on one screen; pushing their
  // tag groups into distant islands just scatters a dozen nodes across acres.
  if (gs < 2 || graph.nodes.length < 25) return;
  const { cx, cy, cnt } = clusterCentroids(graph, gs);
  // Measured, not estimated: the real spread of each cluster decides how far
  // apart two need to sit, so big sprawling folders still get clear water.
  const rmax = new Float64Array(gs);
  for (const n of graph.nodes) {
    const d = Math.hypot(n.x - cx[n.group]!, n.y - cy[n.group]!);
    if (d > rmax[n.group]!) rmax[n.group]! = d;
  }
  const fx = new Float64Array(gs);
  const fy = new Float64Array(gs);
  const radius = (g: number) => Math.min(1200, Math.max(rmax[g]!, Math.sqrt(cnt[g]!) * 30));
  for (let a = 0; a < gs; a++) {
    if (cnt[a]! === 0) continue;
    for (let b = a + 1; b < gs; b++) {
      if (cnt[b]! === 0) continue;
      const dx = cx[a]! - cx[b]!;
      const dy = cy[a]! - cy[b]!;
      const dist = Math.max(1, Math.hypot(dx, dy));
      // The gap must clear both blobs' world-space padding (~34 each) plus
      // visible water between them; small graphs get proportionally less.
      const want = radius(a) + radius(b) + (graph.nodes.length < 60 ? 40 : 150);
      if (dist >= want) continue;
      const push = ((want - dist) / dist) * 0.09 * alpha;
      fx[a]! += dx * push;
      fy[a]! += dy * push;
      fx[b]! -= dx * push;
      fy[b]! -= dy * push;
    }
  }
  for (const n of graph.nodes) {
    if (n.pinned) continue;
    n.vx += fx[n.group]!;
    n.vy += fy[n.group]!;
  }
}

function nudge(n: GNode, fx: number, fy: number) {
  if (n.pinned) return;
  n.vx += fx;
  n.vy += fy;
}

/** Springs along edges: stronger for heavier coupling, so tight clusters
 * physically pull together.
 *
 * Force is normalised by each node's link count (the d3-force trick): a hub
 * with 700 edges must not receive 700 full-strength kicks per tick, or the
 * whole layout accelerates without bound and flies off to ±1e56 — which is
 * exactly what an un-normalised version of this function did on a large
 * dependency graph. */
function applySprings(graph: Graph, alpha: number) {
  for (const e of graph.edges) {
    const a = graph.nodes[e.a]!;
    const b = graph.nodes[e.b]!;
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const dist = Math.max(1, Math.hypot(dx, dy));
    // Cross-cluster edges are long and loose: they say "these talk" without
    // dragging two directories into one heap.
    const cross = a.group !== b.group;
    const target = (cross ? 1.7 : 1) * (92 + a.er + b.er);
    const strength =
      ((cross ? 0.35 : 1) * 0.06 * Math.min(2.4, 0.7 + Math.log2(1 + e.weight) * 0.5)) /
      Math.min(a.links || 1, b.links || 1);
    const f = ((dist - target) / dist) * strength * alpha;
    // The better-connected end moves less; its position is the consensus of
    // many edges, not this one.
    const bias = (b.links || 1) / ((a.links || 1) + (b.links || 1));
    nudge(a, dx * f * bias * 2, dy * f * bias * 2);
    nudge(b, -dx * f * (1 - bias) * 2, -dy * f * (1 - bias) * 2);
  }
}

const CELL = 155;

function bucketKey(x: number, y: number): number {
  return (Math.floor(x / CELL) << 16) ^ (Math.floor(y / CELL) & 0xffff);
}

/** Repulsion via a spatial hash: only near neighbours push, which is both the
 * O(n) trick and the reason distant clusters do not explode apart. */
function applyRepulsion(nodes: GNode[], alpha: number) {
  const buckets = new Map<number, number[]>();
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i]!;
    const key = bucketKey(n.x, n.y);
    const list = buckets.get(key);
    if (list) list.push(i);
    else buckets.set(key, [i]);
  }

  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i]!;
    for (let ox = -1; ox <= 1; ox++) {
      for (let oy = -1; oy <= 1; oy++) {
        const list = buckets.get(bucketKey(n.x + ox * CELL, n.y + oy * CELL));
        if (list) repelWithin(nodes, n, i, list, alpha);
      }
    }
  }
}

function repelWithin(nodes: GNode[], n: GNode, i: number, list: number[], alpha: number) {
  for (const j of list) {
    if (j <= i) continue;
    const m = nodes[j]!;
    const dx = n.x - m.x;
    const dy = n.y - m.y;
    const d2 = Math.max(40, dx * dx + dy * dy);
    if (d2 > CELL * CELL * 2.6) continue;
    let f = (1500 * alpha) / d2;
    const dist = Math.sqrt(d2);
    // Hard separation inside touching distance: overlapping tiles are the
    // single biggest readability killer, so this term stays strong even as
    // alpha cools.
    if (dist < n.er + m.er + 16) f += 0.7;
    nudge(n, (dx / dist) * f, (dy / dist) * f);
    nudge(m, (-dx / dist) * f, (-dy / dist) * f);
  }
}

/** Gravity: mostly toward the node's own group anchor — that is what pulls
 * each directory into a distinct island — plus a whisper toward the centre so
 * unconnected groups stay on screen. The speed limit is the hard guarantee
 * against runaway: whatever the forces do, a node moves a bounded distance
 * per tick, so the layout can only converge. */
function integrate(graph: Graph, alpha: number, cx: number, cy: number) {
  const maxSpeed = 2 + 30 * alpha;
  for (const n of graph.nodes) {
    if (n.pinned) continue;
    const a = graph.anchors[n.group];
    if (a) {
      n.vx += (a.x - n.x) * 0.009 * alpha;
      n.vy += (a.y - n.y) * 0.009 * alpha;
    }
    n.vx += (cx - n.x) * 0.0015 * alpha;
    n.vy += (cy - n.y) * 0.0015 * alpha;
    n.vx *= 0.75;
    n.vy *= 0.75;
    const speed = Math.hypot(n.vx, n.vy);
    if (speed > maxSpeed) {
      n.vx = (n.vx / speed) * maxSpeed;
      n.vy = (n.vy / speed) * maxSpeed;
    }
    n.x += n.vx;
    n.y += n.vy;
  }
}

// ------------------------------------------------------------------ similarity

export interface SimilarHit {
  fileId: string;
  fn: ArchFn;
  score: number;
}

function nameWords(name: string): string[] {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_-]/g, " ")
    .toLowerCase()
    .split(/\s+/)
    .filter((w) => w.length > 1);
}

function overlap(a: readonly string[], b: readonly string[]): number {
  if (a.length === 0 || b.length === 0) return 0;
  const set = new Set(a);
  let hits = 0;
  for (const t of b) if (set.has(t)) hits++;
  return hits / Math.min(a.length, b.length);
}

/**
 * Functions across the codebase that look like this one: shared vocabulary,
 * similar name, shared call targets, same tag. Powers the "highlight similar"
 * interaction — entirely local, no model involved.
 */
/** Turns persisted "path::name" refs back into live hits. */
function resolveStored(doc: ArchDoc, stored: { target: string; score: number }[]): SimilarHit[] {
  const out: SimilarHit[] = [];
  for (const s of stored) {
    const cut = s.target.lastIndexOf("::");
    if (cut <= 0) continue;
    const fid = s.target.slice(0, cut);
    const name = s.target.slice(cut + 2);
    const fn = doc.files.find((f) => f.id === fid)?.functions.find((x) => x.name === name);
    if (fn) out.push({ fileId: fid, fn, score: s.score });
  }
  return out;
}

export function similarFunctions(doc: ArchDoc, fileId: string, fnName: string): SimilarHit[] {
  const home = doc.files.find((f) => f.id === fileId);
  const target = home?.functions.find((f) => f.name === fnName);
  if (!home || !target) return [];

  // The scan precomputes lookalikes into the architecture file; live scoring
  // below only serves maps written before that existed.
  const stored = target.similar ?? [];
  if (stored.length > 0) return resolveStored(doc, stored);

  const tWords = nameWords(target.name);
  const hits: SimilarHit[] = [];

  for (const file of doc.files) {
    for (const fn of file.functions) {
      if (file.id === fileId && fn.name === fnName) continue;
      const score =
        0.5 * overlap(target.tokens, fn.tokens) +
        0.28 * overlap(tWords, nameWords(fn.name)) +
        0.15 * overlap(target.calls, fn.calls) +
        (target.tag && fn.tag === target.tag ? 0.07 : 0);
      if (score >= 0.34) hits.push({ fileId: file.id, fn, score });
    }
  }

  hits.sort((a, b) => b.score - a.score);
  return hits.slice(0, 14);
}
