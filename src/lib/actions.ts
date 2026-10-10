// Shared commands.
//
// Anything reachable from more than one place — a menu, the palette, a keyboard
// shortcut, a link inside a message — lives here so the behaviour is identical
// wherever it is triggered from.

import { api } from "./ipc";
import { store, toast } from "./store";
import { dropSession, getSession } from "./session";
import type { Chat } from "./types";
import { errText } from "~/lib/err";

const IMAGE_EXT = /\.(png|jpe?g|jfif|gif|webp|avif|bmp|ico|tiff?|heic|heif)$/i;
const SVG_EXT = /\.svg$/i;
const HTML_EXT = /\.html?$/i;

/** Turns a chat-written path into the absolute one the viewers need.
 *
 * The file tree hands `openPath` absolute paths, but the model writes paths
 * relative to its working directory — so images and HTML opened FROM THE CHAT
 * arrived at the viewers baseless and broke, while the same file opened from
 * the browser worked. Resolution mirrors the backend's `resolve_cwd`: the
 * chat's cwd, then the project root, then home. */
async function resolveWorkspacePath(raw: string): Promise<string | null> {
  let p = raw.trim().replace(/^\.\//, "");
  if (p.startsWith("~/") || p === "~") {
    const home = await api.fsHome().catch(() => null);
    if (home) p = p === "~" ? home : `${home}/${p.slice(2)}`;
    return p;
  }
  if (p.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(p)) return p;

  const cwd = store.activeChat()?.cwd ?? store.activeProject()?.rootPath ?? null;
  if (cwd?.startsWith("ssh://")) {
    toast("warn", "That file lives on the remote workspace — download it first, or open it in the terminal.");
    return null;
  }
  if (cwd) return `${cwd.replace(/\/+$/, "")}/${p}`;
  const home = await api.fsHome().catch(() => null);
  return home ? `${home}/${p}` : p;
}

/** Opens a path in the right surface for its type. */
export async function openPath(rawPath: string, line?: number) {
  const path = await resolveWorkspacePath(rawPath);
  if (!path) return;
  if (IMAGE_EXT.test(path)) {
    store.openPane({ kind: "image", path });
    return;
  }
  if (SVG_EXT.test(path)) {
    store.openPane({ kind: "svg", path });
    return;
  }
  if (HTML_EXT.test(path)) {
    try {
      const file = await api.fsRead(path);
      store.openPane({ kind: "preview", html: file.content, title: file.name });
      return;
    } catch {
      /* fall through to the editor */
    }
  }
  store.openPane({ kind: "editor", path, line });
}

export async function newChat(projectId?: string | null): Promise<Chat | null> {
  try {
    // An untouched "New chat" in the same scope is this chat — focus it
    // instead of minting an identical twin.
    const existing = store
      .chats()
      .find(
        (c) =>
          c.messageCount === 0 &&
          c.title === "New chat" &&
          (c.projectId ?? null) === (projectId ?? null) &&
          !store.runningChats().has(c.id),
      );
    // Reuse only when it actually changes what is on screen. Pressing "new"
    // while already looking at the empty chat is an explicit ask for another
    // one — a silent no-op there reads as a broken button.
    if (existing && store.activeChatId() !== existing.id) {
      store.setActiveChatId(existing.id);
      return existing;
    }

    const chat = await api.createChat({ projectId: projectId ?? null });
    store.setChats((cs) => [chat, ...cs]);
    store.setActiveChatId(chat.id);
    if (projectId) {
      store.setExpandedProjects((prev) => new Set(prev).add(projectId));
      void store.refreshProjects();
    }
    return chat;
  } catch (e) {
    toast("error", `Could not create chat: ${errText(e)}`);
    return null;
  }
}

export async function newProject(name: string, rootPath?: string) {
  try {
    const p = await api.createProject(name, rootPath);
    store.setProjects((ps) => [...ps, p]);
    store.setExpandedProjects((prev) => new Set(prev).add(p.id));
    return p;
  } catch (e) {
    toast("error", `Could not create project: ${errText(e)}`);
    return null;
  }
}

export async function deleteChat(id: string) {
  try {
    await api.deleteChat(id);
    dropSession(id);
    store.setChats((cs) => cs.filter((c) => c.id !== id));
    if (store.activeChatId() === id) {
      const next = store.chats()[0];
      store.setActiveChatId(next?.id ?? null);
    }
    void store.refreshProjects();
  } catch (e) {
    toast("error", `Could not delete chat: ${errText(e)}`);
  }
}

export async function renameChat(id: string, title: string) {
  const trimmed = title.trim();
  if (!trimmed) return;
  store.setChats((cs) => cs.map((c) => (c.id === id ? { ...c, title: trimmed } : c)));
  try {
    await api.updateChat({ id, title: trimmed });
  } catch (e) {
    toast("error", `Could not rename: ${errText(e)}`);
    void store.refreshChats();
  }
}

export async function setChatModel(id: string, model: string) {
  store.setChats((cs) => cs.map((c) => (c.id === id ? { ...c, model } : c)));
  await api.updateChat({ id, model }).catch(() => void store.refreshChats());
}

export async function pickDirectory(title = "Choose a folder"): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title });
  return typeof picked === "string" ? picked : null;
}

export async function pickFiles(): Promise<string[]> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ multiple: true, title: "Attach files" });
  if (!picked) return [];
  return Array.isArray(picked) ? picked : [picked];
}

const ZOOM_MIN = 0.6;
const ZOOM_MAX = 2;

export async function zoomBy(delta: number) {
  const s = store.settings();
  if (!s) return;
  const next = Math.round(Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, (s.zoom ?? 1) + delta)) * 100) / 100;
  if (next === s.zoom) return;
  await store.setSettings({ ...s, zoom: next });
  toast("info", `Zoom ${Math.round(next * 100)}%`);
}

export async function zoomReset() {
  const s = store.settings();
  if (!s || s.zoom === 1) return;
  await store.setSettings({ ...s, zoom: 1 });
  toast("info", "Zoom 100%");
}

export function cycleTheme() {
  const s = store.settings();
  if (!s) return;
  const order = ["system", "light", "dark"] as const;
  const next = order[(order.indexOf(s.theme) + 1) % order.length]!;
  void store.setSettings({ ...s, theme: next });
  toast("info", `Theme: ${next}`);
}

export function togglePane(kind: "terminal" | "files" | "changes" | "analysis" | "graph") {
  store.togglePaneSurface({ kind });
}

/** ⌘J and the pane button: hide the pane, or bring back what it was showing.
 * With nothing to restore, fall back to the file tree. */
export function toggleSidePane() {
  if (store.paneOpen()) store.closePane();
  else if (!store.reopenPane()) togglePane("files");
}

/** Maximize only makes sense with a pane on screen; opening one first keeps
 * the shortcut meaningful from anywhere. */
export function toggleMaximizedPane() {
  if (!store.paneOpen()) {
    togglePane("analysis");
    store.setPaneMaximized(true);
    return;
  }
  store.setPaneMaximized(!store.paneMaximized());
}

export function toggleBrowser(url = "https://openrouter.ai/models") {
  store.togglePaneSurface({ kind: "browser", url });
}

/** Opens the diff for the most recent change in this chat. */
export async function showLatestDiff() {
  const chatId = store.activeChatId();
  if (!chatId) return;
  const changes = await api.chatChanges(chatId).catch(() => []);
  const latest = changes.find((c) => !c.reverted) ?? changes[0];
  if (latest) store.openPane({ kind: "diff", changeId: latest.latestChangeId });
  else togglePane("changes");
}

export async function stopActiveRun() {
  const id = store.activeChatId();
  if (!id) return;
  await getSession(id).cancel();
}

export async function copyText(text: string, what = "Copied") {
  try {
    await navigator.clipboard.writeText(text);
    toast("ok", what);
  } catch {
    toast("error", "Clipboard is not available");
  }
}

export function formatCost(usd: number): string {
  if (!usd) return "$0";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  if (usd < 1) return `$${usd.toFixed(3)}`;
  return `$${usd.toFixed(2)}`;
}

/**
 * Rough token count for text the provider never billed us for — a user message
 * before it is sent, or a reasoning trace that arrives without a usage frame.
 *
 * Roughly four characters per token holds for prose; code and punctuation run
 * denser, so whitespace-heavy text is nudged down. Always shown with a `~` so
 * it is never mistaken for the exact figures OpenRouter returns.
 */
export function estimateTokens(text: string): number {
  if (!text) return 0;
  const chars = text.length;
  const words = text.split(/\s+/).filter(Boolean).length;
  return Math.max(1, Math.round((chars / 4 + words * 0.75) / 2));
}

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function formatBytes(n: number): string {
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${units[i]}`;
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

export function formatWhen(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  if (sameDay) return d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  const days = Math.floor((now.getTime() - ts) / 86_400_000);
  if (days < 7) return d.toLocaleDateString(undefined, { weekday: "short" });
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
