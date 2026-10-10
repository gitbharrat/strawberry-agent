// Floating side chat.
//
// A small always-on-top panel for the question you do not want to derail the
// main thread with — "what does this flag do", "summarise what we just
// changed". It is scoped to the current project (and inherits its folder), so
// the answers are about the work in front of you rather than a blank slate.
//
// It reuses the ordinary session machinery: the same run loop, the same tools,
// the same journal. Only the chrome is different.

import { createEffect, createSignal, For, on, onMount, Show } from "solid-js";
import { Close, Send, Sparkle, Stop } from "./Icons";
import { Markdown } from "./Markdown";
import { getSession, type Session } from "~/lib/session";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { errText } from "~/lib/err";

const SIDE_TITLE = "Side chat";
const MIN_W = 300;
const MIN_H = 240;

export function SideChat() {
  const [chatId, setChatId] = createSignal<string | null>(null);
  const [text, setText] = createSignal("");
  const [box, setBox] = createSignal(restoreBox());
  let field!: HTMLTextAreaElement;

  const session = (): Session | null => {
    const id = chatId();
    return id ? getSession(id) : null;
  };

  // One side chat per project, created on demand and reused after that.
  async function ensureChat(projectId: string | null) {
    const existing = store.chats().find((c) => c.title === SIDE_TITLE && c.projectId === projectId);
    if (existing) {
      setChatId(existing.id);
      return;
    }
    try {
      const chat = await api.createChat({ projectId, title: SIDE_TITLE });
      store.setChats((cs) => [chat, ...cs]);
      setChatId(chat.id);
    } catch (e) {
      toast("error", `Could not open the side chat: ${errText(e)}`);
    }
  }

  onMount(() => {
    void ensureChat(store.activeProject()?.id ?? null);
    queueMicrotask(() => field?.focus());
  });

  // Follow the project as the user moves between them.
  createEffect(
    on(
      () => store.activeProject()?.id ?? null,
      (projectId) => void ensureChat(projectId),
      { defer: true },
    ),
  );

  createEffect(() => {
    const s = session();
    if (s && !s.loaded()) void s.load();
  });

  const busy = () => {
    const s = session()?.live.status;
    return s !== undefined && s !== "idle" && s !== "error";
  };

  async function send() {
    const s = session();
    const body = text().trim();
    if (!s || !body || busy()) return;
    setText("");
    if (field) field.style.height = "auto";
    try {
      await s.send(body, []);
    } catch (e) {
      setText(body);
      toast("error", errText(e));
    }
  }

  /** Shared handler for the title-bar drag and the corner resize. */
  function grab(kind: "move" | "resize") {
    return (e: PointerEvent) => {
      e.preventDefault();
      const start = { x: e.clientX, y: e.clientY, ...box() };
      const onMove = (ev: PointerEvent) => {
        const dx = ev.clientX - start.x;
        const dy = ev.clientY - start.y;
        setBox(
          kind === "move"
            ? {
                ...start,
                // Keep at least a sliver on screen so it can always be grabbed.
                right: Math.max(8, Math.min(window.innerWidth - 120, start.right - dx)),
                top: Math.max(8, Math.min(window.innerHeight - 48, start.top + dy)),
              }
            : {
                ...start,
                w: Math.max(MIN_W, Math.min(window.innerWidth - 40, start.w - dx)),
                h: Math.max(MIN_H, Math.min(window.innerHeight - start.top - 16, start.h + dy)),
              },
        );
      };
      const onUp = () => {
        persistBox(box());
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    };
  }

  return (
    <div
      class="sidechat"
      style={{
        right: `${box().right}px`,
        top: `${box().top}px`,
        width: `${box().w}px`,
        height: `${box().h}px`,
      }}
    >
      <div class="sidechat-resize" role="presentation" onPointerDown={grab("resize")} />

      <div class="sidechat-head" role="presentation" onPointerDown={grab("move")}>
        <Sparkle size={12} />
        <span class="truncate">{store.activeProject()?.name ?? "Side chat"}</span>
        <div class="spacer" />
        <Show when={busy()}>
          <button
            type="button"
            class="btn"
            data-size="icon-sm"
            title="Stop"
            onClick={() => void session()?.cancel()}
          >
            <Stop size={10} />
          </button>
        </Show>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close  ⌘⇧A"
          onClick={() => store.setSideChatOpen(false)}
        >
          <Close size={11} />
        </button>
      </div>

      <div class="sidechat-body">
        <Show when={session()} fallback={<div class="sidechat-empty muted">Opening…</div>}>
          {(s) => (
            <>
              <For
                each={s().messages.filter((m) => m.role !== "tool")}
                fallback={
                  <div class="sidechat-empty muted">
                    A scratch thread for quick questions about this project. It has the same tools and folder
                    as the main chat.
                  </div>
                }
              >
                {(m) => (
                  <div class="sidechat-msg" data-role={m.role}>
                    <Show when={m.role === "user"} fallback={<Markdown text={m.content} />}>
                      <div class="sidechat-user selectable">{m.content}</div>
                    </Show>
                  </div>
                )}
              </For>
              <Show when={s().live.text}>
                <div class="sidechat-msg" data-role="assistant">
                  <Markdown text={s().live.text} streaming />
                </div>
              </Show>
              <Show when={busy() && !s().live.text}>
                <div class="thinking-dots" role="status" aria-label="Working">
                  <span />
                  <span />
                  <span />
                </div>
              </Show>
            </>
          )}
        </Show>
      </div>

      <div class="sidechat-foot">
        <textarea
          ref={field}
          class="sidechat-input"
          rows={1}
          placeholder="Quick question…"
          value={text()}
          spellcheck={false}
          onInput={(e) => {
            setText(e.currentTarget.value);
            e.currentTarget.style.height = "auto";
            e.currentTarget.style.height = `${Math.min(96, e.currentTarget.scrollHeight)}px`;
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              void send();
            }
            if (e.key === "Escape") store.setSideChatOpen(false);
          }}
        />
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          data-variant="primary"
          disabled={!text().trim() || busy()}
          onClick={() => void send()}
          title="Send"
        >
          <Send size={12} />
        </button>
      </div>
    </div>
  );
}

interface Box {
  right: number;
  top: number;
  w: number;
  h: number;
}

const BOX_KEY = "sa.sidechat.box";

function restoreBox(): Box {
  // Top-right by default, clear of the chat tabs and the topbar.
  const fallback: Box = { right: 16, top: 76, w: 380, h: 460 };
  try {
    const raw = localStorage.getItem(BOX_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<Box>;
    return {
      right: Number(parsed.right) || fallback.right,
      top: Number(parsed.top) || fallback.top,
      w: Math.max(MIN_W, Number(parsed.w) || fallback.w),
      h: Math.max(MIN_H, Number(parsed.h) || fallback.h),
    };
  } catch {
    return fallback;
  }
}

function persistBox(box: Box) {
  try {
    localStorage.setItem(BOX_KEY, JSON.stringify(box));
  } catch {
    /* private mode */
  }
}
