// Global keyboard shortcuts.
//
// One listener on the window rather than per-component handlers, so the
// bindings are all visible in one place and cannot drift between views.

import { store } from "./store";
import {
  cycleTheme,
  newChat,
  showLatestDiff,
  toggleBrowser,
  toggleMaximizedPane,
  togglePane,
  toggleSidePane,
  zoomBy,
  zoomReset,
} from "./actions";
import { getSession } from "./session";

const isMac = navigator.platform.toLowerCase().includes("mac");

function mod(e: KeyboardEvent): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}

/** True when focus is somewhere that should keep the keystroke. */
function inEditableField(e: KeyboardEvent): boolean {
  const el = e.target as HTMLElement | null;
  if (!el) return false;
  return el.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName);
}

export function installHotkeys() {
  // biome-ignore lint/complexity/noExcessiveCognitiveComplexity: a flat keybinding table reads better in one place
  window.addEventListener("keydown", (e) => {
    // Escape closes the topmost overlay, then falls through to the composer's
    // own handler, which stops a run.
    if (e.key === "Escape") {
      if (store.paletteOpen()) {
        e.preventDefault();
        store.setPaletteOpen(false);
        return;
      }
      if (store.settingsOpen()) {
        e.preventDefault();
        store.setSettingsOpen(false);
        return;
      }
    }

    const key = e.key.toLowerCase();

    // Ctrl+` is the terminal on every platform, so it is checked before the
    // platform-specific modifier gate below.
    if (e.ctrlKey && (key === "`" || key === "~")) {
      e.preventDefault();
      togglePane("terminal");
      return;
    }

    if (!mod(e)) return;

    // Zoom. WebKit reports the key as "=" on an unshifted plus, and numeric-pad
    // variants come through as "Add"/"Subtract".
    if (key === "=" || key === "+" || e.code === "NumpadAdd") {
      e.preventDefault();
      void zoomBy(0.1);
      return;
    }
    if (key === "-" || e.code === "NumpadSubtract") {
      e.preventDefault();
      void zoomBy(-0.1);
      return;
    }
    if (key === "0") {
      e.preventDefault();
      void zoomReset();
      return;
    }

    // ⌘1–9 jumps to an open chat tab.
    if (key >= "1" && key <= "9" && !e.shiftKey) {
      e.preventDefault();
      store.focusChatIndex(Number(key) - 1);
      return;
    }

    // Shift-modified pane shortcuts.
    if (e.shiftKey) {
      switch (key) {
        case "f":
          e.preventDefault();
          togglePane("files");
          return;
        case "d":
          e.preventDefault();
          void showLatestDiff();
          return;
        case "b":
          e.preventDefault();
          toggleBrowser();
          return;
        case "l":
          e.preventDefault();
          cycleTheme();
          return;
        case "a":
          e.preventDefault();
          store.setSideChatOpen(!store.sideChatOpen());
          return;
        case "c":
          e.preventDefault();
          togglePane("changes");
          return;
        case "g":
          e.preventDefault();
          togglePane("analysis");
          return;
        case "e":
          e.preventDefault();
          togglePane("graph");
          return;
        case "m":
          e.preventDefault();
          toggleMaximizedPane();
          return;
        case "p":
          e.preventDefault();
          store.togglePaneSurface({ kind: "plan" });
          return;
        case "r":
          e.preventDefault();
          store.togglePaneSurface({ kind: "activity" });
          return;
        case "w":
          // Shift closes the pane tab; plain ⌘W closes the chat tab.
          if (!store.activeTabId()) return;
          e.preventDefault();
          store.closeTab(store.activeTabId()!);
          return;
        default:
          return;
      }
    }

    // Cmd+Enter approves a pending tool: the prompt is modal in attention even
    // though it is not modal on screen.
    if (key === "enter") {
      const chatId = store.activeChatId();
      if (!chatId) return;
      const session = getSession(chatId);
      const pending = session.live.approvalId;
      if (pending) {
        e.preventDefault();
        void session.approve(pending, true);
      }
      return;
    }

    switch (key) {
      case "k":
        e.preventDefault();
        store.setPaletteOpen(!store.paletteOpen());
        break;
      case "n":
        e.preventDefault();
        void newChat(store.activeChat()?.projectId ?? null);
        break;
      case "b":
        e.preventDefault();
        store.setRailCollapsed(!store.railCollapsed());
        break;
      case "j":
        e.preventDefault();
        toggleSidePane();
        break;
      case "w": {
        const chatId = store.activeChatId();
        if (!chatId) return;
        e.preventDefault();
        store.closeChatTab(chatId);
        break;
      }
      case ",":
        e.preventDefault();
        store.setSettingsOpen(true);
        break;
      case "f":
        if (inEditableField(e)) return;
        e.preventDefault();
        store.setPaletteOpen(true);
        break;
      case ".":
        e.preventDefault();
        togglePane("changes");
        break;
      default:
        break;
    }
  });
}
