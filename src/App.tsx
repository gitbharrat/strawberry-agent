// Application shell: rail, chat column, and the resizable side pane.

import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { api } from "~/lib/ipc";
import { Rail } from "./components/Rail";
import { ChatTabs } from "./components/ChatTabs";
import { ChatView } from "./components/ChatView";
import { Pane } from "./components/Pane";
import { CommandPalette } from "./components/CommandPalette";
import { SettingsSheet } from "./components/Settings";
import { SideChat } from "./components/SideChat";
import { Toasts } from "./components/Toasts";
import { Welcome } from "./components/Welcome";
import { Logo } from "./components/Logo";
import { store } from "~/lib/store";
import { newChat } from "~/lib/actions";
import { installHotkeys } from "~/lib/hotkeys";

export function App() {
  onMount(() => {
    installHotkeys();
    void store.init();
  });

  return (
    <>
      <Show when={store.ready()} fallback={<Boot />}>
        <div
          class="app"
          data-rail={store.railCollapsed() ? "collapsed" : "open"}
          style={{ "--rail-w": `${store.railWidth()}px` }}
        >
          <Rail />
          <Show when={!store.railCollapsed()}>
            <RailResizer />
          </Show>
          <main class="main">
            <ChatTabs />
            <Show when={store.keyStatus().configured} fallback={<Welcome />}>
              <Show when={store.activeChatId()} fallback={<NoChat />}>
                {(id) => (
                  <div
                    class="workspace"
                    data-split={store.paneOpen()}
                    data-max={store.paneOpen() && store.paneMaximized()}
                    style={{ "--pane-w": `${store.paneWidth()}%` }}
                  >
                    <ChatView chatId={id()} />
                    {/* Mounted while any tab exists — closed tabs are hidden,
                        not destroyed, so their state survives. The splitter
                        only appears when something is actually visible. */}
                    <Show when={store.paneMounted()}>
                      <Show when={store.paneOpen()}>
                        <Splitter />
                      </Show>
                      <Pane />
                    </Show>
                  </div>
                )}
              </Show>
            </Show>
          </main>
        </div>
      </Show>

      <Show when={store.paletteOpen()}>
        <CommandPalette />
      </Show>
      <Show when={store.settingsOpen()}>
        <SettingsSheet />
      </Show>
      <Show when={store.sideChatOpen()}>
        <SideChat />
      </Show>
      <Toasts />
      <ContextMenu />
    </>
  );
}

/**
 * App right-click menu: Reload and Inspect element.
 *
 * The native menu keeps its territory — anywhere the user can type or has
 * text selected still gets copy/paste; everywhere else, right-click serves
 * the two things a developer actually reaches for.
 */
function ContextMenu() {
  const [pos, setPos] = createSignal<{ x: number; y: number } | null>(null);
  const [hasSelection, setHasSelection] = createSignal(false);
  let ctxTarget: HTMLElement | null = null;

  onMount(() => {
    const onCtx = (e: MouseEvent) => {
      const el = e.target as HTMLElement | null;
      const inEditable = !!el && (el.isContentEditable || ["INPUT", "TEXTAREA"].includes(el.tagName));
      const sel =
        el?.tagName === "TEXTAREA" || el?.tagName === "INPUT"
          ? (el as HTMLTextAreaElement).selectionStart !== (el as HTMLTextAreaElement).selectionEnd
          : (window.getSelection()?.toString() ?? "") !== "";
      e.preventDefault();
      ctxTarget = el;
      setHasSelection(sel || inEditable);
      setPos({
        x: Math.min(e.clientX, window.innerWidth - 170),
        y: Math.min(e.clientY, window.innerHeight - 150),
      });
    };
    const onClose = () => setPos(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPos(null);
    };
    window.addEventListener("contextmenu", onCtx);
    window.addEventListener("click", onClose);
    window.addEventListener("blur", onClose);
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      window.removeEventListener("contextmenu", onCtx);
      window.removeEventListener("click", onClose);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("keydown", onKey);
    });
  });

  // Edit actions run against the still-focused field: mousedown on a menu
  // item is prevented so the click never steals focus from the target.
  async function copySelection() {
    setPos(null);
    const el = document.activeElement as HTMLTextAreaElement | HTMLInputElement | null;
    let text = "";
    if (el && (el.tagName === "TEXTAREA" || el.tagName === "INPUT")) {
      text = el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0);
    } else {
      text = window.getSelection()?.toString() ?? "";
    }
    if (text) {
      const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
      await writeText(text).catch(() => {});
    }
  }

  function selectAll() {
    setPos(null);
    const field = ctxTarget?.closest("input, textarea") as HTMLInputElement | HTMLTextAreaElement | null;
    if (field) {
      field.focus();
      field.select();
      setHasSelection(true);
      return;
    }
    // Outside fields, select the nearest text surface rather than the world.
    const scope = ctxTarget?.closest(".selectable") ?? ctxTarget ?? document.body;
    const sel = window.getSelection();
    if (!sel) return;
    sel.removeAllRanges();
    const range = document.createRange();
    range.selectNodeContents(scope);
    sel.addRange(range);
    setHasSelection(true);
  }

  const keepFocus = (e: MouseEvent) => e.preventDefault();

  return (
    <Show when={pos()}>
      {(p) => (
        <div class="ctxmenu" style={{ left: `${p().x}px`, top: `${p().y}px` }}>
          <button type="button" class="ctxmenu-item" onMouseDown={keepFocus} onClick={selectAll}>
            Select all
          </button>
          <Show when={hasSelection()}>
            <button
              type="button"
              class="ctxmenu-item"
              onMouseDown={keepFocus}
              onClick={() => void copySelection()}
            >
              Copy
            </button>
          </Show>
          <div class="ctxmenu-sep" />
          <button type="button" class="ctxmenu-item" onClick={() => location.reload()}>
            Reload
          </button>
          <button
            type="button"
            class="ctxmenu-item"
            onClick={() => {
              setPos(null);
              void api.openDevtools().catch(() => {});
            }}
          >
            Inspect element
          </button>
        </div>
      )}
    </Show>
  );
}

function Boot() {
  return (
    <div class="boot">
      <Logo size={44} />
    </div>
  );
}

function NoChat() {
  return (
    <div class="empty">
      <Logo size={40} />
      <h2>No chat selected</h2>
      <p>Pick one from the sidebar, or start a new one.</p>
      <div class="empty-actions">
        <button type="button" class="btn" data-variant="primary" onClick={() => void newChat(null)}>
          New chat
        </button>
        <button type="button" class="btn" data-variant="outline" onClick={() => store.setPaletteOpen(true)}>
          Search <kbd>⌘</kbd>
          <kbd>K</kbd>
        </button>
      </div>
    </div>
  );
}

/**
 * Rail resizer: a thin grab strip on the rail's right edge. Width applies live
 * through the CSS variable and persists on release.
 */
function RailResizer() {
  const onPointerDown = (e: PointerEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = store.railWidth();
    const onMove = (ev: PointerEvent) => store.setRailWidth(startW + (ev.clientX - startX));
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };
  return <div class="rail-resizer" role="presentation" onPointerDown={onPointerDown} />;
}

/**
 * Pane resizer.
 *
 * Width is written straight to the CSS custom property during the drag and
 * only committed to the store on release, so dragging never re-runs the
 * component tree.
 */
function Splitter() {
  const [dragging, setDragging] = createSignal(false);

  const onPointerDown = (e: PointerEvent) => {
    e.preventDefault();
    setDragging(true);
    const workspace = (e.currentTarget as HTMLElement).parentElement!;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);

    let latest = store.paneWidth();
    const onMove = (ev: PointerEvent) => {
      const rect = workspace.getBoundingClientRect();
      const pct = ((rect.right - ev.clientX) / rect.width) * 100;
      latest = Math.min(72, Math.max(22, pct));
      workspace.style.setProperty("--pane-w", `${latest}%`);
    };
    const onUp = () => {
      setDragging(false);
      store.setPaneWidth(latest);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };

  return <div class="splitter" data-dragging={dragging()} onPointerDown={onPointerDown} />;
}
