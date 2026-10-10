// Global application state.
//
// Solid signals rather than a store framework: the whole app is a handful of
// lists plus a few flags, and fine-grained signals mean a chat-title change
// touches one text node instead of re-running a tree.

import { createSignal, createRoot, batch } from "solid-js";
import { api } from "./ipc";
import { errorMessage } from "./types";
import type { AppInfo, Chat, KeyStatus, ModelInfo, Project, Settings, ToolSummary } from "./types";
import { errText } from "~/lib/err";

export type PaneSurface =
  | { kind: "terminal" }
  | { kind: "activity" }
  | { kind: "plan" }
  | { kind: "files" }
  | { kind: "changes" }
  | { kind: "analysis" }
  | { kind: "graph" }
  | { kind: "sketch" }
  | { kind: "browser"; url: string }
  | { kind: "editor"; path: string; line?: number }
  | { kind: "diff"; changeId: string }
  | { kind: "preview"; html: string; title: string }
  | { kind: "image"; path: string }
  | { kind: "svg"; path: string };

export interface PaneTab {
  id: string;
  surface: PaneSurface;
  title: string;
  /** Hide sequence when closed-but-mounted; undefined while visible. Closed
   * tabs keep their component alive (terminal PTYs, canvas cameras) and the
   * sequence orders eviction of the oldest hidden documents. */
  hiddenAt?: number;
}

/** Surfaces that are cheap, per-document views. Hidden ones beyond a small
 * cap are truly closed — unlike the stateful singletons (terminal,
 * architecture, graph…), nothing of value is lost. */
const DOC_KINDS = new Set(["editor", "diff", "image", "svg", "preview"]);
const HIDDEN_DOC_CAP = 8;

/**
 * Identity of a surface for tab reuse.
 *
 * Opening the same file twice focuses the tab that is already showing it;
 * singleton surfaces like the terminal and the file tree collapse to one tab
 * each, so the strip never fills up with duplicates.
 */
function surfaceKey(s: PaneSurface): string {
  switch (s.kind) {
    case "editor":
      return `editor:${s.path}`;
    case "diff":
      return `diff:${s.changeId}`;
    case "image":
      return `image:${s.path}`;
    case "svg":
      return `svg:${s.path}`;
    case "preview":
      return `preview:${s.title}`;
    case "browser":
      return "browser";
    default:
      return s.kind;
  }
}

function surfaceTitle(s: PaneSurface): string {
  switch (s.kind) {
    case "terminal":
      return "Terminal";
    case "activity":
      return "Activity";
    case "plan":
      return "Plan";
    case "files":
      return "Files";
    case "changes":
      return "Changes";
    case "analysis":
      return "Architecture";
    case "graph":
      return "Graph";
    case "sketch":
      return "Sketch";
    case "browser":
      return "Browser";
    case "editor":
      return s.path.split("/").pop() ?? s.path;
    case "diff":
      return "Diff";
    case "preview":
      return s.title;
    case "image":
    case "svg":
      return s.path.split("/").pop() ?? s.path;
  }
}

function createStore() {
  const [ready, setReady] = createSignal(false);
  const [appInfo, setAppInfo] = createSignal<AppInfo | null>(null);
  const [settings, setSettingsRaw] = createSignal<Settings | null>(null);
  const [keyStatus, setKeyStatus] = createSignal<KeyStatus>({
    configured: false,
    masked: null,
    store: "file",
    path: "",
  });

  const [projects, setProjects] = createSignal<Project[]>([]);
  const [chats, setChats] = createSignal<Chat[]>([]);
  const [activeChatId, setActiveChatIdRaw] = createSignal<string | null>(null);
  // Chats open as tabs across the top, the way an editor holds open files.
  const [openChats, setOpenChats] = createSignal<string[]>([]);
  const [expandedProjects, setExpandedProjects] = createSignal<Set<string>>(new Set());

  const [models, setModels] = createSignal<ModelInfo[]>([]);
  const [modelsStale, setModelsStale] = createSignal(false);
  const [modelsError, setModelsError] = createSignal<string | null>(null);
  const [modelsLoading, setModelsLoading] = createSignal(false);
  const [tools, setTools] = createSignal<ToolSummary[]>([]);

  // Chats with a live run. Drives the pulse in the rail and the run bar.
  const [runningChats, setRunningChats] = createSignal<Set<string>>(new Set());

  // ---- UI
  const [railCollapsed, setRailCollapsed] = createSignal(false);
  const [railWidth, setRailWidthRaw] = createSignal(restoreInt("sa.railw", 236, 180, 420));
  const [paneTabs, setPaneTabs] = createSignal<PaneTab[]>([]);
  const [activeTabId, setActiveTabId] = createSignal<string | null>(null);
  const [paneWidth, setPaneWidthRaw] = createSignal(restoreInt("sa.panew", 44, 22, 72));
  // The side pane can take over the whole workspace — a graph or a diff wants
  // the full window, and the chat column keeps its state hidden underneath.
  const [paneMaximized, setPaneMaximized] = createSignal(false);

  function setRailWidth(px: number) {
    const v = Math.min(420, Math.max(180, Math.round(px)));
    setRailWidthRaw(v);
    persistInt("sa.railw", v);
  }

  function setPaneWidth(pct: number) {
    const v = Math.min(72, Math.max(22, Math.round(pct)));
    setPaneWidthRaw(v);
    persistInt("sa.panew", v);
  }
  const [paletteOpen, setPaletteOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [settingsTab, setSettingsTab] = createSignal<string>("general");

  /** Opens settings on a specific tab — "ssh", "models", "agent"… */
  function openSettings(tab?: string) {
    if (tab) setSettingsTab(tab);
    setSettingsOpen(true);
  }
  const [sideChatOpen, setSideChatOpen] = createSignal(false);
  const [toasts, setToasts] = createSignal<Toast[]>([]);

  /** Focusing a chat opens a tab for it if one is not already open. */
  function setActiveChatId(id: string | null) {
    setActiveChatIdRaw(id);
    if (!id) return;
    setOpenChats((tabs) => (tabs.includes(id) ? tabs : [...tabs, id]));
  }

  function closeChatTab(id: string) {
    const tabs = openChats();
    const index = tabs.indexOf(id);
    if (index === -1) return;
    const next = tabs.filter((t) => t !== id);
    setOpenChats(next);
    if (activeChatId() === id) {
      // Focus the neighbour, preferring the one to the left.
      setActiveChatIdRaw(next[Math.max(0, index - 1)] ?? null);
    }
  }

  function focusChatIndex(n: number) {
    const id = openChats()[n];
    if (id) setActiveChatIdRaw(id);
  }

  const activeChat = () => chats().find((c) => c.id === activeChatId()) ?? null;
  const activeProject = () => {
    const c = activeChat();
    return c?.projectId ? (projects().find((p) => p.id === c.projectId) ?? null) : null;
  };

  function applyTheme(s: Settings) {
    const root = document.documentElement;
    const dark =
      s.theme === "dark" || (s.theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
    root.dataset.theme = dark ? "dark" : "light";
    root.dataset.density = s.density;
    root.dataset.motion = s.reduceMotion ? "off" : "on";
    root.style.setProperty("--fs-base", `${s.uiFontSize}px`);
    root.style.setProperty("--content-w", `${s.contentWidth || 820}px`);
    // WebKit scales the whole layout crisply from the root, so zoom needs no
    // per-token maths and never blurs text the way a transform would.
    root.style.zoom = String(s.zoom ?? 1);
    try {
      localStorage.setItem("sa.theme", s.theme);
    } catch {
      /* private mode */
    }
  }

  async function setSettings(next: Settings) {
    // Apply optimistically: the UI should never lag a preference toggle.
    setSettingsRaw(next);
    applyTheme(next);
    try {
      setSettingsRaw(await api.setSettings(next));
    } catch (e) {
      toast("error", `Could not save settings: ${errText(e)}`);
    }
  }

  async function refreshProjects() {
    setProjects(await api.projects());
  }

  async function refreshChats() {
    const next = await api.chats(null);
    setChats(next);
    const live = new Set(next.map((c) => c.id));
    setOpenChats((tabs) => tabs.filter((id) => live.has(id)));
  }

  async function refreshModels(force = false) {
    setModelsLoading(true);
    try {
      const cat = await api.models(force);
      batch(() => {
        setModels(cat.models);
        setModelsStale(cat.stale);
        setModelsError(null);
      });
    } catch (e) {
      // Not fatal — a model id can still be typed in — but the picker has to
      // say why it is empty rather than looking like nothing matched.
      setModelsError(errorMessage(e));
    } finally {
      setModelsLoading(false);
    }
  }

  async function refreshKeyStatus() {
    setKeyStatus(await api.keyStatus());
  }

  function markRunning(chatId: string, running: boolean) {
    setRunningChats((prev) => {
      const next = new Set(prev);
      if (running) next.add(chatId);
      else next.delete(chatId);
      return next;
    });
  }

  function toggleProject(id: string) {
    setExpandedProjects((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  const activeTab = () => paneTabs().find((t) => t.id === activeTabId()) ?? null;
  /** Tabs shown in the strip. Hidden tabs stay mounted but out of sight. */
  const visiblePaneTabs = () => paneTabs().filter((t) => t.hiddenAt === undefined);
  const paneOpen = () => visiblePaneTabs().length > 0;
  /** Any tab at all, hidden included — the pane host must stay mounted for
   * hidden tabs to keep their state. */
  const paneMounted = () => paneTabs().length > 0;

  // What the SSH machinery is doing right now, rendered as a chip beside the
  // ssh button: busy stays until replaced, ok clears itself, errors persist
  // until dismissed or superseded.
  const [sshStatus, setSshStatus] = createSignal<{ text: string; tone: "busy" | "ok" | "error" } | null>(
    null,
  );
  let sshStatusTimer: ReturnType<typeof setTimeout> | undefined;
  function reportSsh(text: string, tone: "busy" | "ok" | "error") {
    if (sshStatusTimer) clearTimeout(sshStatusTimer);
    setSshStatus({ text, tone });
    if (tone === "ok") sshStatusTimer = setTimeout(() => setSshStatus(null), 6000);
  }
  function clearSshStatus() {
    if (sshStatusTimer) clearTimeout(sshStatusTimer);
    setSshStatus(null);
  }

  // An SSH connection asked for before the terminal pane exists. A plain
  // window event raced the pane's mount and simply vanished on first open;
  // a signal is consumed whenever the pane is ready, whichever comes first.
  const [pendingSsh, setPendingSsh] = createSignal<{
    kind: "host" | "alias";
    value: string;
    dir?: string;
  } | null>(null);

  /** Opens a surface, reusing an existing tab — hidden or visible — when it
   * is the same thing. A hidden tab comes back exactly as it was left. */
  function openPane(surface: PaneSurface) {
    const key = surfaceKey(surface);
    const existing = paneTabs().find((t) => t.id === key);
    if (existing) {
      // Refresh the payload: reopening a file at a new line should scroll there.
      setPaneTabs((tabs) => tabs.map((t) => (t.id === key ? { ...t, surface, hiddenAt: undefined } : t)));
      setActiveTabId(key);
      return;
    }
    setPaneTabs((tabs) => [...tabs, { id: key, surface, title: surfaceTitle(surface) }]);
    setActiveTabId(key);
  }

  /** Opens if absent, closes if it is already the focused tab. */
  function togglePaneSurface(surface: PaneSurface) {
    const key = surfaceKey(surface);
    if (activeTabId() === key) closeTab(key);
    else openPane(surface);
  }

  let hideSeq = 0;
  /** Evicts the oldest hidden document tabs past the cap. Stateful singletons
   * (terminal, architecture…) are never evicted. */
  function gcHiddenDocs() {
    setPaneTabs((tabs) => {
      const hiddenDocs = tabs
        .filter((t) => t.hiddenAt !== undefined && DOC_KINDS.has(t.surface.kind))
        .sort((a, b) => (a.hiddenAt ?? 0) - (b.hiddenAt ?? 0));
      if (hiddenDocs.length <= HIDDEN_DOC_CAP) return tabs;
      const evict = new Set(hiddenDocs.slice(0, hiddenDocs.length - HIDDEN_DOC_CAP).map((t) => t.id));
      return tabs.filter((t) => !evict.has(t.id));
    });
  }

  /** "Closing" a tab hides it: the component stays mounted, so terminals keep
   * their PTYs and the architecture map its camera. Reopening the surface
   * unhides the same tab. */
  function closeTab(id: string) {
    const vis = visiblePaneTabs();
    const index = vis.findIndex((t) => t.id === id);
    if (index === -1) return;
    setPaneTabs((tabs) => tabs.map((t) => (t.id === id ? { ...t, hiddenAt: ++hideSeq } : t)));
    gcHiddenDocs();
    const next = vis.filter((t) => t.id !== id);
    if (activeTabId() === id) {
      // Focus the neighbour, preferring the one to the left.
      setActiveTabId(next[Math.max(0, index - 1)]?.id ?? null);
    }
    if (next.length === 0) setPaneMaximized(false);
  }

  // What closePane hid, so reopenPane can bring the same set back.
  let stashedPane: { ids: string[]; active: string | null } | null = null;

  /** Hides the whole pane. Everything stays mounted; `reopenPane` restores. */
  function closePane() {
    const vis = visiblePaneTabs();
    if (vis.length > 0) stashedPane = { ids: vis.map((t) => t.id), active: activeTabId() };
    setPaneTabs((tabs) => tabs.map((t) => (t.hiddenAt === undefined ? { ...t, hiddenAt: ++hideSeq } : t)));
    setActiveTabId(null);
    setPaneMaximized(false);
  }

  /** Restores the tabs the last closePane hid. False when there is nothing
   * to restore (caller decides what to open instead). */
  function reopenPane(): boolean {
    const stash = stashedPane;
    stashedPane = null;
    if (!stash) return false;
    const have = new Set(paneTabs().map((t) => t.id));
    const ids = stash.ids.filter((id) => have.has(id));
    if (ids.length === 0) return false;
    setPaneTabs((tabs) => tabs.map((t) => (ids.includes(t.id) ? { ...t, hiddenAt: undefined } : t)));
    setActiveTabId(stash.active && ids.includes(stash.active) ? stash.active : (ids[ids.length - 1] ?? null));
    return true;
  }

  async function init() {
    const [info, s, keys] = await Promise.all([api.appInfo(), api.getSettings(), api.keyStatus()]);
    batch(() => {
      setAppInfo(info);
      setSettingsRaw(s);
      setKeyStatus(keys);
    });
    applyTheme(s);

    // Follow the OS when the user has not pinned a theme.
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      const cur = settings();
      if (cur?.theme === "system") applyTheme(cur);
    });

    await Promise.all([refreshProjects(), refreshChats()]);
    setReady(true);

    // Non-blocking: the app is usable before these land.
    void refreshModels();
    void api
      .tools()
      .then(setTools)
      .catch(() => {});
    void api
      .runningChatIds()
      .then((ids) => setRunningChats(new Set(ids)))
      .catch(() => {});
  }

  return {
    ready,
    init,
    appInfo,
    settings,
    setSettings,
    applyTheme,
    keyStatus,
    setKeyStatus,
    refreshKeyStatus,
    projects,
    setProjects,
    refreshProjects,
    chats,
    setChats,
    refreshChats,
    activeChatId,
    setActiveChatId,
    activeChat,
    activeProject,
    openChats,
    setOpenChats,
    closeChatTab,
    focusChatIndex,
    expandedProjects,
    toggleProject,
    setExpandedProjects,
    models,
    modelsStale,
    modelsError,
    modelsLoading,
    refreshModels,
    tools,
    runningChats,
    markRunning,
    railCollapsed,
    setRailCollapsed,
    railWidth,
    setRailWidth,
    paneTabs,
    setPaneTabs,
    visiblePaneTabs,
    activeTabId,
    setActiveTabId,
    activeTab,
    paneOpen,
    paneMounted,
    openPane,
    reopenPane,
    pendingSsh,
    setPendingSsh,
    sshStatus,
    reportSsh,
    clearSshStatus,
    togglePaneSurface,
    closeTab,
    closePane,
    paneWidth,
    setPaneWidth,
    paneMaximized,
    setPaneMaximized,
    paletteOpen,
    setPaletteOpen,
    settingsOpen,
    setSettingsOpen,
    settingsTab,
    setSettingsTab,
    openSettings,
    sideChatOpen,
    setSideChatOpen,
    toasts,
    setToasts,
  };
}

/** localStorage int with bounds; storage failures fall back silently. */
function restoreInt(key: string, fallback: number, min: number, max: number): number {
  try {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v >= min && v <= max ? v : fallback;
  } catch {
    return fallback;
  }
}

function persistInt(key: string, v: number) {
  try {
    localStorage.setItem(key, String(v));
  } catch {
    /* private mode */
  }
}

export interface Toast {
  id: number;
  level: "info" | "ok" | "warn" | "error";
  text: string;
  action?: { label: string; run: () => void };
}

let toastSeq = 0;

export const store = createRoot(createStore);

export function toast(level: Toast["level"], text: string, action?: Toast["action"]) {
  const id = ++toastSeq;
  store.setToasts((t) => [...t, { id, level, text, action }]);
  const ttl = level === "error" ? 9000 : 4200;
  setTimeout(() => store.setToasts((t) => t.filter((x) => x.id !== id)), ttl);
}

export function dismissToast(id: number) {
  store.setToasts((t) => t.filter((x) => x.id !== id));
}
