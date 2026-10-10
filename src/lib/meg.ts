// The Model Execution Graph: types, the node catalogue, and the built-in
// templates.
//
// A graph is a Fusion-style flow of stages standing between the user's message
// and the final answer: research feeds planning, planning feeds execution,
// verification loops back until the work passes. Every node carries its own
// model, reasoning effort and a premade instruction, so a cheap fast model can
// do the digging while a strong one does the building — that split, plus
// cached node outputs, is what makes a graph *cheaper* than one big run.
//
// This file is pure data and geometry-free helpers. The executor lives in
// Rust (`src-tauri/src/meg.rs` — these types mirror its structs exactly);
// the canvas lives in `components/GraphPane`.

export type NodeKind =
  | "goal"
  | "research"
  | "fetch"
  | "codebase"
  | "environment"
  | "archmap"
  | "mapupdate"
  | "plan"
  | "architecture"
  | "design"
  | "sketch"
  | "brainstorm"
  | "judge"
  | "execute"
  | "optimize"
  | "refactor"
  | "documentation"
  | "test"
  | "verify"
  | "critique"
  | "edgecases"
  | "merge"
  | "gate"
  | "output"
  | "note";

/** Out-port of an edge. Plain nodes have one `out`; verdict nodes (verify,
 * test) expose `pass`/`fail`, and a gate exposes `approve`/`reject`. */
export type OutPort = "out" | "pass" | "fail" | "approve" | "reject";

export interface NodeConfig {
  /** Tool reach for agent-backed nodes. */
  tools?: "none" | "read" | "web" | "edit" | "full";
  /** Brainstorm: how many independent variants to produce. */
  fanout?: number;
  /** Include the original user request in this node's context (default true). */
  includeGoal?: boolean;
  /** Verdict nodes: what to do when the loop budget runs dry. */
  onExhaust?: "continue" | "fail";
  /** auto = re-run when inputs change · always = fresh every time ·
   * once = at most once per run, so loop passes keep serving its result. */
  rerun?: "auto" | "always" | "once";
  /** Include the last N chat messages as context for this node. */
  history?: number;
  /** Agent nodes may search past conversations (memory tools). */
  memory?: boolean;
}

/** A visual grouping — Fusion's underlay. Collapsed, it renders as one tile
 * (a saved custom node lands this way); execution ignores groups entirely. */
export interface GraphGroup {
  id: string;
  label: string;
  collapsed: boolean;
}

export interface GraphNode {
  id: string;
  kind: NodeKind;
  /** User-facing name on the tile; defaults to the kind's title. */
  label: string;
  x: number;
  y: number;
  /** null → the chat's model. */
  model: string | null;
  /** Reasoning effort for this node: null → global setting, "off" → none. */
  effort: string | null;
  /** The premade small message — the node's standing instruction, editable.
   * An empty prompt on a non-agent node turns it into a free pass-through. */
  prompt: string;
  /** Disabled nodes pass their inputs straight through (Fusion's bypass). */
  enabled: boolean;
  /** Pinned nodes always serve their cached output and never re-run — how a
   * research brief or a colour theme gets reused across runs for free. */
  pinned: boolean;
  /** Membership in a visual group, if any. */
  group?: string | null;
  config: NodeConfig;
}

export interface GraphEdge {
  id: string;
  from: string;
  fromPort: OutPort;
  to: string;
  /** A loop edge points back up the flow. The executor re-runs the loop body
   * at most `maxIters` times; the UI draws it dashed with the budget badge. */
  loop: boolean;
  maxIters: number;
}

export interface GraphSpec {
  id: string;
  name: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
  groups: GraphGroup[];
}

/** A node's cached output from the last run that produced one. */
export interface NodeArtifact {
  /** Content hash of everything that shaped this output; a matching hash lets
   * the executor serve the cache instead of paying for the call again. */
  hash: string;
  content: string;
  /** Structured verdict for verify/test nodes, JSON-encoded. */
  verdict: string | null;
  model: string | null;
  tokensIn: number;
  tokensOut: number;
  cost: number;
  elapsedMs: number;
  createdAt: number;
}

export interface GraphDoc {
  id: string;
  projectId: string | null;
  name: string;
  spec: GraphSpec;
  artifacts: Record<string, NodeArtifact>;
  updatedAt: number;
}

export interface Verdict {
  pass: boolean;
  score: number | null;
  issues: string[];
  summary: string;
}

export type NodeRunStatus = "pending" | "waiting" | "running" | "done" | "cached" | "failed" | "skipped";

// ------------------------------------------------------------- the catalogue

export type NodeCategory = "source" | "context" | "reasoning" | "design" | "action" | "quality" | "flow";

export interface NodeTypeInfo {
  kind: NodeKind;
  title: string;
  category: NodeCategory;
  /** One-glyph face, the Fusion idiom: read the job from the tile. */
  glyph: string;
  /** What this node is for — shown in the add-menu and the inspector. */
  blurb: string;
  /** The premade small message a fresh node starts with. */
  prompt: string;
  /** True when the node runs the real tool-using agent rather than one call. */
  agentic: boolean;
  /** Verdict nodes expose pass/fail ports and can gate loops. */
  verdict: boolean;
  /** Free nodes never cost tokens. */
  free?: boolean;
  defaults: NodeConfig;
}

export const NODE_TYPES: NodeTypeInfo[] = [
  {
    kind: "goal",
    title: "Goal",
    category: "source",
    glyph: "▶",
    blurb:
      "The entry point. With a prompt it restates the request as verifiable acceptance criteria; with an empty prompt it passes the message through for free.",
    prompt:
      "Restate the request as a goal statement: what is being built or answered, the constraints that matter, and 3–7 concrete acceptance criteria a reviewer could check. Do not solve anything yet. Be brief — this is a brief, not an essay.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "research",
    title: "Research",
    category: "context",
    glyph: "◎",
    blurb:
      "Searches the web for prior art, references, techniques and examples relevant to the goal, and distils them into a brief.",
    prompt:
      "Research this task online. Find prior art, reference implementations, established techniques, and concrete examples worth imitating. Prefer primary sources. Distil what you find into a tight brief: key techniques with one-line explanations, notable examples with what makes each good, pitfalls to avoid, and links. No filler.",
    agentic: true,
    verdict: false,
    defaults: { tools: "web" },
  },
  {
    kind: "fetch",
    title: "Fetch",
    category: "context",
    glyph: "⇣",
    blurb:
      "Acquires the raw materials the task needs from the internet — downloads reference images, example code, libraries, datasets and test fixtures into the workspace, and reports a manifest of what landed where. Research explains; fetch DELIVERS files.",
    prompt:
      "Gather the raw materials this task needs from the internet — do not summarise, ACQUIRE:\n" +
      "- Reference images and visual examples: search for them, download the good ones into assets/reference/ (or a sensible workspace folder), LOOK at each with view_image to confirm it is genuinely right, and share the best with share_image so the user sees what you chose.\n" +
      "- Example code and prior implementations: locate the canonical repo or snippet, clone or download it into a scratch folder (e.g. .fetch/), and name the exact files worth imitating.\n" +
      "- Libraries and packages: identify the right library and version for this stack, install it with the project's package manager (or record the exact install command if installing is unsafe here), and link its docs.\n" +
      "- Data, fixtures and test inputs: download realistic samples (a photo to edit, its mask, a CSV to parse, an API response) the later stages can run against.\n" +
      "Never fabricate a file URL from memory — copy exact URLs out of search results or the hosting page (fetch_url the page first); a guessed CDN path is how downloads 404. Verify every download actually landed (list the files, check sizes, view_image the images; re-fetch what failed). Finish with a manifest: each item — what it is, the local path, the source URL, licence if visible, and one line on why it was chosen. Downstream stages use your manifest verbatim, so exact paths matter.",
    agentic: true,
    verdict: false,
    defaults: { tools: "full" },
  },
  {
    kind: "codebase",
    title: "Codebase",
    category: "context",
    glyph: "⌂",
    blurb:
      "Reads the workspace — files, structure, conventions — and reports exactly what the task will touch.",
    prompt:
      "Investigate the workspace as it relates to the goal. Identify the files and functions involved, the conventions and patterns the change must follow, and anything that would surprise an implementer. Report: relevant files with one-line roles, the integration points, constraints, and open questions. Read code; do not modify anything.",
    agentic: true,
    verdict: false,
    defaults: { tools: "read" },
  },
  {
    kind: "environment",
    title: "Environment",
    category: "context",
    glyph: "⎈",
    blurb:
      "Works out what this environment is and how code actually runs here — and when it cannot run, how to test it anyway.",
    prompt:
      "Determine how code is built, run and tested in this environment. Inspect manifests, lockfiles, scripts, CI configs and toolchains; identify the exact commands for build, run and test, and verify the runtimes exist. If code cannot be executed here (missing runtime, sandbox, no hardware), say so plainly and prescribe the best substitute verification: static checks, type checks, dry runs, unit-level reasoning, or a minimal harness worth creating. Output a short operator's guide the later stages can follow verbatim.",
    agentic: true,
    verdict: false,
    defaults: { tools: "read" },
  },
  {
    kind: "archmap",
    title: "Arch map",
    category: "context",
    glyph: "⌘",
    blurb:
      "Emits the saved architecture map — files, functions, complexity, lint flags, dead code — for free. Wire it into Optimize or Refactor.",
    prompt: "",
    agentic: false,
    verdict: false,
    free: true,
    defaults: {},
  },
  {
    kind: "mapupdate",
    title: "Map update",
    category: "context",
    glyph: "⟳",
    blurb:
      "Re-scans the workspace and refreshes the architecture map, then emits it. Only changed functions cost tokens.",
    prompt: "",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "plan",
    title: "Plan",
    category: "reasoning",
    glyph: "≣",
    blurb: "Turns goal plus context into an ordered, checkable plan of work.",
    prompt:
      "Write the implementation plan: ordered steps small enough to verify, each naming what changes and how to check it. Call out the riskiest step and the order that de-risks it earliest. Where inputs disagree, resolve the conflict explicitly. Keep it under 30 lines.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "architecture",
    title: "Architecture",
    category: "reasoning",
    glyph: "◫",
    blurb: "Designs the technical shape: modules, data flow, interfaces, state.",
    prompt:
      "Design the technical architecture for this goal: the components, what owns which state, the data flow between them, and the interfaces (signatures, schemas, formats) precisely enough to build against. Prefer the simplest structure that survives the stated requirements. Note the one or two decisions most likely to be wrong and what would falsify them.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "design",
    title: "Design",
    category: "design",
    glyph: "❖",
    blurb:
      "Sets the visual direction: what makes design good for this problem, plus a concrete colour theme emitted as swatches.",
    prompt:
      'Set the visual direction for this goal. First: what makes design good for this specific kind of artifact — name the 3–5 principles that matter here (hierarchy, contrast, spacing, motion, materiality…) and how each applies. Then commit to one direction: mood, typography guidance, spacing scale, a concrete colour theme, and — when the goal has a visual surface — the layout as a wireframe. End with exactly one fenced ```json block: {"theme": {"name": string, "colors": [{"role": string, "hex": "#rrggbb"}]}, "wireframe": {"name": string, "cols": 12, "rows": 8, "areas": [{"label": string, "x": int, "y": int, "w": int, "h": int, "role": "nav"|"hero"|"content"|"media"|"sidebar"|"footer"|"cta"}]}}. Give 5–8 colours covering background, surface, text and accents with accessible contrast; place the key regions on the 12×8 grid; omit "wireframe" only when nothing visual is being made.',
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "sketch",
    title: "Sketch",
    category: "design",
    glyph: "✏",
    blurb:
      "No AI, no tokens: the run pauses and the sketch pad opens for YOU. Draw the wireframe, or paste a screenshot/mockup and annotate over it — your drawing's layout flows downstream. Cached, so loops and re-runs reuse the sketch instead of asking again.",
    prompt:
      "Draw what you want: boxes for regions, arrows for moves, text for anything the builder must honour. Paste a screenshot to annotate over it.",
    agentic: false,
    verdict: false,
    free: true,
    defaults: { rerun: "once" },
  },
  {
    kind: "brainstorm",
    title: "Brainstorm",
    category: "reasoning",
    glyph: "✳",
    blurb: "Generates several genuinely different approaches before anything is committed.",
    prompt:
      "Produce 3 genuinely different approaches to the goal — different in strategy, not phrasing. For each: the core idea in two lines, what it does best, its main risk, and its cost. Do not pick a winner; that is the judge's job.",
    agentic: false,
    verdict: false,
    defaults: { fanout: 3 },
  },
  {
    kind: "judge",
    title: "Judge",
    category: "reasoning",
    glyph: "⚖",
    blurb: "Compares alternatives from upstream and commits to one, stealing the best parts of the losers.",
    prompt:
      "Judge the alternatives in your inputs against the goal. Score each briefly on fitness, risk and cost, pick exactly one winner, and fold in any losing ideas worth keeping. Output the committed approach as a single coherent brief the next stage can act on without reading the alternatives.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "execute",
    title: "Execute",
    category: "action",
    glyph: "▸",
    blurb:
      "The hands: the full tool-using agent that does the actual work in the workspace, guided by everything upstream.",
    prompt:
      "Carry out the work. Follow the plan and honour the architecture, design and research you were given — they are decisions, not suggestions. Build it properly: run what you build, fix what fails. When done, summarise what was created or changed, file by file, and how it satisfies the acceptance criteria. If feedback from a verifier is present, fix exactly what it raises before anything else.",
    agentic: true,
    verdict: false,
    defaults: { tools: "full" },
  },
  {
    kind: "optimize",
    title: "Optimize",
    category: "action",
    glyph: "⚡",
    blurb:
      "Takes the architecture map and refines the hot spots: high-complexity functions, bad algorithmic cost.",
    prompt:
      "Optimize this codebase using the architecture map in your inputs. Target the functions the map flags for complexity and poor big-O first, weighted toward anything relevant to the goal. For each: read it, reduce its complexity or algorithmic cost, and keep behaviour identical — smallest change that genuinely improves it, following the file's conventions. Verify with tests or the build where possible. Report each function as before → after with one line on why it is better.",
    agentic: true,
    verdict: false,
    defaults: { tools: "edit" },
  },
  {
    kind: "refactor",
    title: "Refactor",
    category: "action",
    glyph: "♺",
    blurb: "Takes the architecture map and cleans house: lint flags, dead code, near-duplicate functions.",
    prompt:
      "Refactor this codebase using the architecture map in your inputs. Fix what the linters flag; remove code the map marks dead — but verify each candidate is truly unreferenced (grep for it) before deleting; merge near-duplicate functions into one shared implementation where the similarity is real. Behaviour must not change. Follow existing conventions, keep diffs minimal, and report every fix and every deletion with the proof it was safe.",
    agentic: true,
    verdict: false,
    defaults: { tools: "edit" },
  },
  {
    kind: "documentation",
    title: "Docs",
    category: "action",
    glyph: "¶",
    blurb: "Writes or updates the documentation for what the graph built or changed.",
    prompt:
      "Document the work described in your inputs. Update whatever documentation this project actually keeps — README, docs folder, doc comments — to match reality: what exists now, how to use it, how to run it. Follow the project's existing documentation style and depth; do not invent new documentation conventions. Report which files you touched and why.",
    agentic: true,
    verdict: false,
    defaults: { tools: "edit" },
  },
  {
    kind: "test",
    title: "Test",
    category: "quality",
    glyph: "✓",
    blurb: "Runs the thing — tests, build, a real invocation — and reports a machine-readable pass/fail.",
    prompt:
      "Test the work. Run the test suite, the build, or a real invocation — whichever exists and proves the most; if an environment guide is in your inputs, use its commands. Report what you ran and what happened, quoting failures exactly. Finish with the verdict JSON you were asked for; pass only if what you ran actually succeeded.",
    agentic: true,
    verdict: true,
    defaults: { tools: "full", onExhaust: "continue" },
  },
  {
    kind: "verify",
    title: "Verify",
    category: "quality",
    glyph: "◈",
    blurb:
      "Exercises the work for real — runs it, screenshots UIs, builds harnesses, downloads or generates test inputs — then gates the loop: fail routes back with evidence, pass moves on. Wire notes (design principles, checklists) into it as criteria.",
    prompt:
      "Verify by exercising the work, never by reading alone.\n" +
      "1. Assemble the bar: the goal's acceptance criteria plus every requirement in the reference inputs wired into you (design research, principle notes, checklists) — each becomes a concrete check. If you are unsure how this kind of artifact is normally tested, research the standard practice online first and follow it.\n" +
      "2. Run the real thing, using the environment guide if one is in your inputs. Anything with a visual surface: serve or open it and capture web_screenshot evidence at desktop (1280) and mobile (390) widths — the screenshots are shown to the user — and judge the rendered result, not the source.\n" +
      "3. Nothing runs on its own? Build the harness: write a minimal test or invocation, and obtain realistic inputs — download fixtures from the web (download_file) or generate them yourself (run_python: draw the mask, synthesize the sample). Run it and compare actual output to expected. Inspect any image involved with view_image — judge pixels, never filenames.\n" +
      "4. Report every defect as `severity: what is wrong — how to fix it`, each tied to its evidence (screenshot, output, log line). Pass only if what you actually ran meets the goal and every wired-in criterion.",
    agentic: true,
    verdict: true,
    defaults: { tools: "full", onExhaust: "continue" },
  },
  {
    kind: "critique",
    title: "Critique",
    category: "quality",
    glyph: "✎",
    blurb: "A non-gating review: what would make this better, fed forward as guidance.",
    prompt:
      "Critique the work in your inputs against the goal. Name the three highest-leverage improvements — concrete, specific, ranked. Also note what is good and must not be broken. This guides a refinement pass, so write it as instructions, not observations.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "edgecases",
    title: "Edge cases",
    category: "quality",
    glyph: "△",
    blurb:
      "Enumerates the edge cases, failure modes and likely bugs for this specific task before or after building.",
    prompt:
      "Enumerate the edge cases and likely failure modes for this specific goal: boundary values, empty and huge inputs, concurrency and timing, malformed data, platform quirks, and misuse. For each: the scenario, why it breaks a naive implementation, and how to handle or test it. Rank by likelihood × damage. Skip generic advice.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "merge",
    title: "Merge",
    category: "flow",
    glyph: "⋈",
    blurb: "Joins several branches into one bundle — free, no model call.",
    prompt: "",
    agentic: false,
    verdict: false,
    free: true,
    defaults: {},
  },
  {
    kind: "gate",
    title: "Gate",
    category: "flow",
    glyph: "‖",
    blurb:
      "Pauses the run and shows you the work so far; you approve, or reject with a note that flows back.",
    prompt: "",
    agentic: false,
    verdict: true,
    free: true,
    defaults: {},
  },
  {
    kind: "output",
    title: "Output",
    category: "flow",
    glyph: "◉",
    blurb: "The terminus: composes the final answer to the user from everything that reached it.",
    prompt:
      "Compose the final response to the user. Report what was built or found, the key decisions and why, how the result was verified, and anything left open. Write directly to the user, in clean markdown. Do not narrate the pipeline — present the outcome.",
    agentic: false,
    verdict: false,
    defaults: {},
  },
  {
    kind: "note",
    title: "Note",
    category: "flow",
    glyph: "▤",
    blurb:
      "A text block you write. Wire its output into a stage — verify especially — to hand it the text as standing context: design principles, checklists, references. Unwired, it is a plain sticky note.",
    prompt: "",
    agentic: false,
    verdict: false,
    free: true,
    defaults: {},
  },
];

export const NODE_TYPE: Record<NodeKind, NodeTypeInfo> = Object.fromEntries(
  NODE_TYPES.map((t) => [t.kind, t]),
) as Record<NodeKind, NodeTypeInfo>;

/** Category hues, keyed to the app palette: colour is meaning, and each stage
 * family keeps one hue across tiles, wires and the legend — the Fusion rule. */
export const CATEGORY_COLOR: Record<NodeCategory, string> = {
  source: "#8c8c8c",
  context: "#6892c0", // steel — looking things up
  reasoning: "#9585d6", // violet — thinking
  design: "#ff8f9f", // berry-300 — aesthetics
  action: "#d99a34", // amber — doing
  quality: "#3fd082", // leaf — checking
  flow: "#7a7a7a",
};

export function outPorts(node: GraphNode): OutPort[] {
  // A note feeds its text down the wire — that is what makes it context.
  if (node.kind === "note") return ["out"];
  if (node.kind === "gate") return ["approve", "reject"];
  return NODE_TYPE[node.kind].verdict ? ["pass", "fail"] : ["out"];
}

export function hasInput(node: GraphNode): boolean {
  // The arch-map node is a pure source; a map *update* accepts an input so it
  // can be sequenced after the work it should re-index.
  return node.kind !== "goal" && node.kind !== "note" && node.kind !== "archmap";
}

// ------------------------------------------------------------------ helpers

export function nodeId(): string {
  return Math.random().toString(36).slice(2, 8);
}

export function makeNode(kind: NodeKind, x: number, y: number): GraphNode {
  const t = NODE_TYPE[kind];
  return {
    id: nodeId(),
    kind,
    label: t.title,
    x,
    y,
    model: null,
    effort: null,
    prompt: t.prompt,
    enabled: true,
    pinned: false,
    group: null,
    config: { ...t.defaults },
  };
}

/** Forward edges only — the DAG the scheduler runs on. */
export function forwardEdges(spec: GraphSpec): GraphEdge[] {
  return spec.edges.filter((e) => !e.loop);
}

/** Would adding from→to create a cycle through forward edges? If so the edge
 * must be a loop edge — that is what a loop *is* here. */
export function wouldCycle(spec: GraphSpec, from: string, to: string): boolean {
  if (from === to) return true;
  const out = new Map<string, string[]>();
  for (const e of forwardEdges(spec)) {
    const list = out.get(e.from);
    if (list) list.push(e.to);
    else out.set(e.from, [e.to]);
  }
  // A cycle appears iff `from` is already reachable from `to`.
  const seen = new Set<string>([to]);
  const stack = [to];
  while (stack.length) {
    const cur = stack.pop()!;
    if (cur === from) return true;
    for (const next of out.get(cur) ?? []) {
      if (!seen.has(next)) {
        seen.add(next);
        stack.push(next);
      }
    }
  }
  return false;
}

/** Kahn's count: true when the forward graph (loop edges excluded) is a DAG. */
function forwardIsAcyclic(spec: GraphSpec, runnable: GraphNode[], ids: Set<string>): boolean {
  const indeg = new Map<string, number>();
  const out = new Map<string, string[]>();
  for (const n of runnable) indeg.set(n.id, 0);
  for (const e of forwardEdges(spec)) {
    if (!ids.has(e.from) || !ids.has(e.to)) continue;
    indeg.set(e.to, (indeg.get(e.to) ?? 0) + 1);
    const list = out.get(e.from);
    if (list) list.push(e.to);
    else out.set(e.from, [e.to]);
  }
  const queue = runnable.filter((n) => (indeg.get(n.id) ?? 0) === 0).map((n) => n.id);
  let visited = 0;
  while (queue.length) {
    const cur = queue.shift()!;
    visited++;
    for (const next of out.get(cur) ?? []) {
      const d = (indeg.get(next) ?? 0) - 1;
      indeg.set(next, d);
      if (d === 0) queue.push(next);
    }
  }
  return visited === runnable.length;
}

/** Structural problems that make a graph unrunnable, in human words. */
export function validateSpec(spec: GraphSpec): string[] {
  const problems: string[] = [];
  // Wired notes take part in the flow (their text runs down the wire);
  // unwired notes are decoration and stay out of every check.
  const wired = new Set(spec.edges.filter((e) => !e.loop).map((e) => e.from));
  const runnable = spec.nodes.filter((n) => n.kind !== "note" || wired.has(n.id));
  const executable = runnable.filter((n) => n.kind !== "note");
  if (executable.length === 0) {
    problems.push("The graph has no nodes.");
    return problems;
  }
  const ids = new Set(runnable.map((n) => n.id));
  if (spec.edges.some((e) => !ids.has(e.from) || !ids.has(e.to))) {
    problems.push("An edge points at a missing node.");
  }

  const hasIncoming = new Set(forwardEdges(spec).map((e) => e.to));
  const entries = executable.filter((n) => !hasIncoming.has(n.id) && n.enabled);
  if (entries.length === 0) problems.push("No entry point — every node has something upstream of it.");

  if (!forwardIsAcyclic(spec, runnable, ids)) {
    problems.push("The flow runs in a circle — mark the backward arrow as a loop, or remove it.");
  }
  if (spec.edges.some((e) => e.loop && !wouldCycle(spec, e.from, e.to))) {
    problems.push("A loop arrow points at a node that is not upstream of it.");
  }
  return problems;
}

export interface WireArea {
  label: string;
  x: number;
  y: number;
  w: number;
  h: number;
  role?: string;
}

/** Every place a JSON payload might hide in a model reply: fenced blocks of
 * any language (last first, where conclusions live), then a bare balanced
 * object around the needle for models that skip the fence entirely. */
function jsonCandidates(content: string, needle: string): string[] {
  const out: string[] = [];
  const fences = content.match(/```[a-zA-Z]*\s*([\s\S]*?)```/g);
  if (fences) {
    for (let i = fences.length - 1; i >= 0; i--) {
      const body = fences[i]!.replace(/^```[a-zA-Z]*/, "").replace(/```$/, "");
      if (body.includes(needle)) out.push(body);
    }
  }
  const bare = objectAround(content, needle);
  if (bare) out.push(bare);
  return out;
}

/** The outermost balanced object containing `needle`, walking outward from
 * the nearest opening brace so nesting cannot fool it. */
function objectAround(content: string, needle: string): string | null {
  const at = content.lastIndexOf(needle);
  if (at === -1) return null;
  let start = content.lastIndexOf("{", at);
  for (let tries = 0; start !== -1 && tries < 5; tries++) {
    let depth = 0;
    for (let i = start; i < content.length; i++) {
      const ch = content[i];
      if (ch === "{") depth++;
      else if (ch === "}") {
        depth--;
        if (depth === 0) {
          const obj = content.slice(start, i + 1);
          if (obj.includes(needle)) return obj;
          break;
        }
      }
    }
    start = content.lastIndexOf("{", start - 1);
  }
  return null;
}

function normalizeHex(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const v = raw.trim().replace(/^([0-9a-fA-F]{3}([0-9a-fA-F]{3})?)$/, "#$1");
  return /^#[0-9a-fA-F]{3}$|^#[0-9a-fA-F]{6}$|^#[0-9a-fA-F]{8}$/.test(v) ? v : null;
}

/** The design node's wireframe ("bluewire") layout, if its output has one. */
export function parseWireframe(
  content: string,
): { name: string; cols: number; rows: number; areas: WireArea[] } | null {
  for (const body of jsonCandidates(content, '"wireframe"')) {
    try {
      const parsed = JSON.parse(body) as {
        wireframe?: { name?: string; cols?: number; rows?: number; areas?: WireArea[] };
      };
      const wf = parsed.wireframe ?? (body.includes('"areas"') ? (JSON.parse(body) as never) : undefined);
      if (!wf?.areas?.length) continue;
      const cols = Math.max(1, Math.min(24, wf.cols ?? 12));
      const rows = Math.max(1, Math.min(24, wf.rows ?? 8));
      const areas = wf.areas
        .filter((a) => typeof a?.label === "string")
        .map((a) => ({
          label: a.label,
          x: Math.max(0, Math.min(cols, Number(a.x) || 0)),
          y: Math.max(0, Math.min(rows, Number(a.y) || 0)),
          w: Math.max(1, Math.min(cols, Number(a.w) || 1)),
          h: Math.max(1, Math.min(rows, Number(a.h) || 1)),
          role: a.role,
        }));
      if (areas.length) return { name: wf.name ?? "Wireframe", cols, rows, areas };
    } catch {
      /* try the next candidate */
    }
  }
  return null;
}

/** The design node's colour theme. Tolerant by design: any fenced block or
 * bare JSON object, `hex` or `color` keys, missing `#` — and when no JSON
 * theme exists at all, the hex codes mentioned in the prose become the
 * palette, so a design that names colours always shows them. */
export function parseTheme(
  content: string,
): { name: string; colors: { role: string; hex: string }[] } | null {
  for (const body of jsonCandidates(content, '"theme"')) {
    try {
      const parsed = JSON.parse(body) as {
        theme?: { name?: string; colors?: { role?: string; hex?: string; color?: string }[] };
      };
      const theme = parsed.theme;
      if (theme?.colors?.length) {
        const colors = theme.colors
          .map((c) => {
            const hex = normalizeHex(c.hex ?? c.color);
            return hex ? { role: c.role ?? hex, hex } : null;
          })
          .filter((c): c is { role: string; hex: string } => c !== null);
        if (colors.length) return { name: theme.name ?? "Theme", colors };
      }
    } catch {
      /* try the next candidate */
    }
  }
  // No structured theme: scrape the prose for colour codes.
  const hexes = [...new Set(content.match(/#[0-9a-fA-F]{6}\b|#[0-9a-fA-F]{3}\b/g) ?? [])].slice(0, 8);
  if (hexes.length >= 3) return { name: "Palette", colors: hexes.map((hex) => ({ role: hex, hex })) };
  return null;
}

/** The design node's editable JSON, as stored inside its fenced block. */
export interface DesignJson {
  theme?: { name?: string; colors?: { role?: string; hex?: string; color?: string }[] };
  wireframe?: {
    name?: string;
    cols?: number;
    rows?: number;
    areas?: { label?: string; x?: number; y?: number; w?: number; h?: number; role?: string }[];
  };
}

/** Rewrites the design JSON block inside a node's artifact content in place —
 * the hand-edit path. Returns null when no editable block exists (a design
 * that only named colours in prose cannot be structurally edited). */
export function updateDesignJson(content: string, mutate: (spec: DesignJson) => void): string | null {
  const re = /```(?:json|design)?[^\n]*\n([\s\S]*?)```/g;
  for (const m of content.matchAll(re)) {
    const body = m[1] ?? "";
    if (!body.includes('"theme"') && !body.includes('"wireframe"')) continue;
    try {
      const spec = JSON.parse(body) as DesignJson;
      mutate(spec);
      const block = `\`\`\`json\n${JSON.stringify(spec, null, 2)}\n\`\`\``;
      return content.slice(0, m.index) + block + content.slice((m.index ?? 0) + m[0].length);
    } catch {
      /* try the next block */
    }
  }
  return null;
}

/** The first fenced design JSON block's body in `content`, or null — used to
 * match a chat message's design against a graph artifact holding the same. */
export function designBlockOf(content: string): string | null {
  const re = /```(?:json|design)?[^\n]*\n([\s\S]*?)```/g;
  for (const m of content.matchAll(re)) {
    const body = m[1] ?? "";
    if (body.includes('"theme"') || body.includes('"wireframe"')) return body;
  }
  return null;
}

// -------------------------------------------------------------- auto planner

/** A reusable fragment the user saved — "research + design as one box". Lives
 * in localStorage; the graph pane owns persistence, the planner reuses them. */
export interface CustomNodeDef {
  name: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

/** The planner model's raw output — loose by design; materialisation cleans. */
export interface AutoPlan {
  name?: string;
  rationale?: string;
  nodes?: {
    id?: string | number;
    kind?: string;
    label?: string | null;
    prompt?: string | null;
    model?: string | null;
    effort?: string | null;
    tools?: string | null;
  }[];
  edges?: {
    from?: string | number;
    to?: string | number;
    port?: string;
    loop?: boolean;
    maxIters?: number;
  }[];
}

/** Where a planner node's edges attach: one real node, or a custom fragment's
 * natural entry and exit members. */
interface PlanEnds {
  entryOf: Map<string, string>;
  exitOf: Map<string, string>;
}

/** Turns a planner reply into a real spec: unknown kinds dropped, stock
 * prompts filled in, ports validated, cycles forced into loop edges, layout
 * computed. `customs` lets the planner reuse the user's saved composite nodes
 * via `kind: "custom:<name>"`, stamped in as collapsed groups. Null when
 * nothing usable survives. */
export function materializePlan(plan: AutoPlan, customs: CustomNodeDef[] = []): GraphSpec | null {
  const spec: GraphSpec = {
    id: nodeId() + nodeId(),
    name: (typeof plan.name === "string" && plan.name.trim() ? plan.name.trim() : "Auto graph").slice(0, 48),
    nodes: [],
    edges: [],
    groups: [],
  };
  const ends = materializeNodes(spec, plan, customs);
  if (spec.nodes.filter((n) => n.kind !== "note").length === 0) return null;
  materializeEdges(spec, plan, ends);
  layoutSpec(spec);
  return spec;
}

function materializeNodes(spec: GraphSpec, plan: AutoPlan, customs: CustomNodeDef[]): PlanEnds {
  const ends: PlanEnds = { entryOf: new Map(), exitOf: new Map() };
  for (const raw of plan.nodes ?? []) {
    if (!raw || typeof raw.kind !== "string") continue;
    const pid = String(raw.id ?? nodeId());
    const customName = raw.kind.startsWith("custom:") ? raw.kind.slice(7).trim() : null;
    if (customName) {
      const def = customs.find((c) => c.name.toLowerCase() === customName.toLowerCase());
      if (!def || def.nodes.length === 0) continue;
      const stamped = stampCustomDef(spec, def);
      ends.entryOf.set(pid, stamped.entry);
      ends.exitOf.set(pid, stamped.exit);
      continue;
    }
    if (!(raw.kind in NODE_TYPE)) continue;
    const node = makeNode(raw.kind as NodeKind, 0, 0);
    applyPlanOverrides(node, raw);
    spec.nodes.push(node);
    ends.entryOf.set(pid, node.id);
    ends.exitOf.set(pid, node.id);
  }
  return ends;
}

function applyPlanOverrides(node: GraphNode, raw: NonNullable<AutoPlan["nodes"]>[number]): void {
  if (typeof raw.label === "string" && raw.label.trim()) node.label = raw.label.trim().slice(0, 40);
  if (typeof raw.prompt === "string") node.prompt = raw.prompt;
  if (typeof raw.model === "string" && raw.model.trim()) node.model = raw.model.trim();
  if (typeof raw.effort === "string" && ["off", "low", "medium", "high"].includes(raw.effort)) {
    node.effort = raw.effort;
  }
  if (typeof raw.tools === "string" && ["none", "read", "web", "edit", "full"].includes(raw.tools)) {
    node.config.tools = raw.tools as NodeConfig["tools"];
  }
}

function materializeEdges(spec: GraphSpec, plan: AutoPlan, ends: PlanEnds): void {
  const byId = new Map(spec.nodes.map((n) => [n.id, n]));
  for (const raw of plan.edges ?? []) {
    const from = ends.exitOf.get(String(raw.from ?? ""));
    const to = ends.entryOf.get(String(raw.to ?? ""));
    if (!from || !to || from === to) continue;
    const source = byId.get(from);
    const target = byId.get(to);
    if (!source || !target || !hasInput(target)) continue;
    const ports = outPorts(source);
    const port: OutPort =
      typeof raw.port === "string" && (ports as string[]).includes(raw.port)
        ? (raw.port as OutPort)
        : (ports[0] ?? "out");
    if (spec.edges.some((e) => e.from === from && e.to === to && e.fromPort === port)) continue;
    // The structure decides loop-ness, not the planner's flag: a backward
    // arrow must be a loop or the graph cannot run.
    const loop = wouldCycle(spec, from, to);
    spec.edges.push({
      id: nodeId(),
      from,
      fromPort: port,
      to,
      loop,
      maxIters: Math.max(1, Math.min(9, Number(raw.maxIters) || 3)),
    });
  }
}

/** Copies a saved fragment into the spec as a collapsed group with fresh ids,
 * returning the member edges from outside should attach to. */
function stampCustomDef(spec: GraphSpec, def: CustomNodeDef): { entry: string; exit: string } {
  const idMap = new Map<string, string>();
  const gid = nodeId();
  const nodes = def.nodes.map((n) => {
    const fresh = nodeId();
    idMap.set(n.id, fresh);
    return { ...structuredClone(n), id: fresh, group: gid };
  });
  const edges = def.edges
    .filter((e) => idMap.has(e.from) && idMap.has(e.to))
    .map((e) => ({
      ...structuredClone(e),
      id: nodeId(),
      from: idMap.get(e.from) ?? e.from,
      to: idMap.get(e.to) ?? e.to,
    }));
  spec.nodes.push(...nodes);
  spec.edges.push(...edges);
  spec.groups.push({ id: gid, label: def.name, collapsed: true });

  const fed = new Set(edges.filter((e) => !e.loop).map((e) => e.to));
  const entry = nodes.find((n) => !fed.has(n.id) && hasInput(n)) ?? nodes[0]!;
  const sources = new Set(edges.filter((e) => !e.loop).map((e) => e.from));
  const exit = nodes.find((n) => !sources.has(n.id) && n.kind !== "note") ?? nodes[nodes.length - 1]!;
  return { entry: entry.id, exit: exit.id };
}

/** Positions nodes in longest-path layers, left to right. */
function layoutSpec(spec: GraphSpec): void {
  const depth = new Map<string, number>(spec.nodes.map((n) => [n.id, 0]));
  // Bellman-Ford-ish relaxation; the forward graph is a DAG so |V| passes settle it.
  for (let i = 0; i < spec.nodes.length; i++) {
    for (const e of forwardEdges(spec)) {
      const d = (depth.get(e.from) ?? 0) + 1;
      if (d > (depth.get(e.to) ?? 0)) depth.set(e.to, d);
    }
  }
  const columns = new Map<number, GraphNode[]>();
  for (const n of spec.nodes) {
    const d = depth.get(n.id) ?? 0;
    const col = columns.get(d);
    if (col) col.push(n);
    else columns.set(d, [n]);
  }
  for (const [d, list] of columns) {
    list.forEach((n, i) => {
      n.x = 40 + d * 240;
      n.y = 60 + i * 110 + (d % 2) * 26;
    });
  }
}

// ---------------------------------------------------------------- templates

export interface GraphTemplate {
  key: string;
  name: string;
  blurb: string;
  build: () => GraphSpec;
}

interface TplNode {
  kind: NodeKind;
  x: number;
  y: number;
  label?: string;
  prompt?: string;
  config?: NodeConfig;
}

/** Compact template builder: nodes by index, edges as [from, to, port?, loopIters?]. */
function tpl(name: string, nodes: TplNode[], edges: [number, number, OutPort?, number?][]): GraphSpec {
  const built = nodes.map((n) => {
    const node = makeNode(n.kind, n.x, n.y);
    if (n.label) node.label = n.label;
    if (n.prompt !== undefined) node.prompt = n.prompt;
    if (n.config) node.config = { ...node.config, ...n.config };
    return node;
  });
  return {
    id: nodeId() + nodeId(),
    name,
    nodes: built,
    edges: edges.map(([f, t, port, iters]) => ({
      id: nodeId(),
      from: built[f]!.id,
      fromPort: port ?? "out",
      to: built[t]!.id,
      loop: iters !== undefined,
      maxIters: iters ?? 3,
    })),
    groups: [],
  };
}

export const TEMPLATES: GraphTemplate[] = [
  {
    key: "essential",
    name: "Essential",
    blurb: "Build → verify → loop until it passes. The default: four nodes, one loop, no wasted calls.",
    build: () =>
      tpl(
        "Essential",
        [
          // The goal passes through for free here — the simplest graph spends
          // its tokens on hands and eyes only.
          { kind: "goal", x: 40, y: 120, prompt: "" },
          { kind: "execute", x: 280, y: 120 },
          { kind: "verify", x: 520, y: 120 },
          { kind: "output", x: 760, y: 120 },
        ],
        [
          [0, 1],
          [1, 2],
          [2, 1, "fail", 2],
          [2, 3, "pass"],
        ],
      ),
  },
  {
    key: "full",
    name: "Full build",
    blurb: "Research, codebase and environment recon in parallel; plan, design, build; test + verify gates.",
    build: () =>
      tpl(
        "Full build",
        [
          { kind: "goal", x: 40, y: 210 },
          { kind: "research", x: 280, y: 60 },
          { kind: "codebase", x: 280, y: 210 },
          { kind: "environment", x: 280, y: 360 },
          { kind: "plan", x: 520, y: 210 },
          { kind: "design", x: 760, y: 60 },
          { kind: "execute", x: 1000, y: 210 },
          { kind: "test", x: 1240, y: 110 },
          { kind: "verify", x: 1240, y: 310 },
          { kind: "output", x: 1480, y: 210 },
        ],
        [
          [0, 1],
          [0, 2],
          [0, 3],
          [1, 4],
          [2, 4],
          [3, 4],
          [4, 5],
          [4, 6],
          [5, 6],
          [6, 7],
          [6, 8],
          [7, 6, "fail", 2],
          [8, 6, "fail", 3],
          [7, 9, "pass"],
          [8, 9, "pass"],
        ],
      ),
  },
  {
    key: "design",
    name: "Design first",
    blurb: "For things that must look good: design research and a colour theme before a line is written.",
    build: () =>
      tpl(
        "Design first",
        [
          { kind: "goal", x: 40, y: 160 },
          {
            kind: "research",
            x: 280,
            y: 160,
            label: "Design research",
            prompt:
              "Research how this kind of artifact is made to look good: standout examples, the visual techniques behind them (palettes, lighting, proportion, composition), and what separates the pleasing ones from the crude ones. Distil into concrete, imitable guidance with references.",
          },
          { kind: "design", x: 520, y: 60 },
          { kind: "plan", x: 520, y: 260 },
          { kind: "execute", x: 760, y: 160 },
          {
            kind: "verify",
            x: 1000,
            y: 160,
            prompt:
              "Verify by looking at the real thing, not the source. Serve or open what was built and capture web_screenshot evidence at 1280 and 390 widths. Judge it against the goal, the design research and the theme: is the palette actually applied, the hierarchy right, the layout faithful to the wireframe? List every concrete deviation as `severity: what is wrong — how to fix it`, tied to a screenshot. Pass only if it genuinely looks right, not merely runs.",
          },
          { kind: "output", x: 1240, y: 160 },
        ],
        [
          [0, 1],
          [1, 2],
          [1, 3],
          [2, 4],
          [3, 4],
          [4, 5],
          [5, 4, "fail", 3],
          [5, 6, "pass"],
        ],
      ),
  },
  {
    key: "report",
    name: "Research report",
    blurb: "Two research angles merged, gap-checked, written up. No code tools involved.",
    build: () =>
      tpl(
        "Research report",
        [
          { kind: "goal", x: 40, y: 160 },
          {
            kind: "research",
            x: 280,
            y: 60,
            label: "Broad sweep",
            prompt:
              "Survey this topic broadly online: the landscape, the main schools of thought or product families, and the strongest sources. Breadth over depth — map the territory and say which corners deserve a closer look.",
          },
          {
            kind: "research",
            x: 280,
            y: 260,
            label: "Deep dive",
            prompt:
              "Go deep on the most load-bearing specifics of this topic: exact numbers, dates, mechanisms, benchmarks, first-hand accounts. Primary sources only where possible; quote precisely and link everything.",
          },
          { kind: "merge", x: 520, y: 160 },
          {
            kind: "critique",
            x: 760,
            y: 160,
            prompt:
              "Review the research for gaps: claims without sources, missing perspectives, stale data, unanswered parts of the goal. List what a sceptical reader would still ask.",
          },
          {
            kind: "output",
            x: 1000,
            y: 160,
            prompt:
              "Write the final report: a clear structure, findings with their sources, honest uncertainty where the research was thin. Address every part of the goal. Clean markdown, no pipeline narration.",
          },
        ],
        [
          [0, 1],
          [0, 2],
          [1, 3],
          [2, 3],
          [3, 4],
          [4, 5],
        ],
      ),
  },
  {
    key: "bughunt",
    name: "Bug hunt",
    blurb: "Diagnose from the code, enumerate edge cases, fix, and re-test until green.",
    build: () =>
      tpl(
        "Bug hunt",
        [
          { kind: "goal", x: 40, y: 160 },
          {
            kind: "codebase",
            x: 280,
            y: 160,
            label: "Locate",
            prompt:
              "Investigate the reported problem in the workspace. Find where the behaviour lives, trace the path that produces it, and identify the most likely cause with evidence (file, line, reasoning). Read only.",
          },
          { kind: "edgecases", x: 520, y: 60 },
          {
            kind: "execute",
            x: 760,
            y: 160,
            label: "Fix",
            prompt:
              "Fix the diagnosed problem. Smallest correct change; follow the codebase's conventions. Guard the edge cases you were given. Summarise the change and why it is right.",
          },
          { kind: "test", x: 1000, y: 160 },
          { kind: "output", x: 1240, y: 160 },
        ],
        [
          [0, 1],
          [1, 2],
          [1, 3],
          [2, 3],
          [3, 4],
          [4, 3, "fail", 3],
          [4, 5, "pass"],
        ],
      ),
  },
  {
    key: "health",
    name: "Code health",
    blurb: "Refactor from the architecture map, re-test, optimize the hot spots, re-test, refresh the map.",
    build: () =>
      tpl(
        "Code health",
        [
          { kind: "goal", x: 40, y: 160, prompt: "" },
          { kind: "archmap", x: 40, y: 300 },
          { kind: "refactor", x: 280, y: 160 },
          { kind: "test", x: 520, y: 160, label: "Test refactor" },
          { kind: "optimize", x: 760, y: 160 },
          { kind: "test", x: 1000, y: 160, label: "Test optimize" },
          { kind: "mapupdate", x: 1240, y: 300 },
          { kind: "output", x: 1240, y: 160 },
        ],
        [
          [0, 2],
          [1, 2],
          [1, 4],
          [2, 3],
          [3, 2, "fail", 2],
          [3, 4, "pass"],
          [4, 5],
          [5, 4, "fail", 2],
          [5, 7, "pass"],
          [5, 6, "pass"],
        ],
      ),
  },
];
