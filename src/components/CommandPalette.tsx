// Command palette.
//
// The primary way to get anywhere: chats, commands and message search in one
// list. Ranking is a small subsequence scorer that rewards prefix and
// word-boundary hits, which is what makes short queries land on the obvious
// answer instead of an alphabetical one.

import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import {
  Chat as ChatIcon,
  Command as CommandIcon,
  Folder,
  Search,
  Settings as SettingsIcon,
  Terminal,
} from "./Icons";
import { api } from "~/lib/ipc";
import { store } from "~/lib/store";
import {
  cycleTheme,
  newChat,
  pickDirectory,
  showLatestDiff,
  toggleBrowser,
  toggleMaximizedPane,
  togglePane,
} from "~/lib/actions";
import type { SearchHit } from "~/lib/types";
import { errText } from "~/lib/err";

interface Item {
  id: string;
  label: string;
  hint?: string;
  group: string;
  icon: "chat" | "command" | "search" | "folder" | "settings" | "terminal";
  run: () => void;
}

/**
 * Subsequence score. Higher is better; -1 means no match.
 * Consecutive runs and word-start hits score highest, so "nc" beats its way to
 * "New chat" rather than to the first alphabetical thing containing n and c.
 */
function score(text: string, query: string): number {
  if (!query) return 0;
  const t = text.toLowerCase();
  const q = query.toLowerCase();
  if (t === q) return 10_000;
  if (t.startsWith(q)) return 5000 - t.length;

  let ti = 0;
  let total = 0;
  let streak = 0;
  for (let qi = 0; qi < q.length; qi++) {
    const ch = q[qi]!;
    let found = -1;
    while (ti < t.length) {
      if (t[ti] === ch) {
        found = ti;
        break;
      }
      ti++;
    }
    if (found === -1) return -1;
    const atBoundary = found === 0 || /[\s/._-]/.test(t[found - 1]!);
    streak = ti > 0 && total > 0 && t[found - 1] === q[qi - 1] ? streak + 1 : 0;
    total += 10 + (atBoundary ? 12 : 0) + streak * 6;
    ti++;
  }
  return total - t.length * 0.1;
}

export function CommandPalette() {
  const [query, setQuery] = createSignal("");
  const [cursor, setCursor] = createSignal(0);
  const [hits, setHits] = createSignal<SearchHit[]>([]);
  let input!: HTMLInputElement;
  let listEl!: HTMLDivElement;

  onMount(() => queueMicrotask(() => input.focus()));

  // Message search is a round trip, so it is debounced and only for real queries.
  createEffect(
    on(query, (q) => {
      if (q.trim().length < 2) {
        setHits([]);
        return;
      }
      const handle = setTimeout(() => {
        void api
          .search(q.trim(), 20)
          .then(setHits)
          .catch(() => {});
      }, 130);
      onCleanup(() => clearTimeout(handle));
    }),
  );

  const commands = (): Item[] => [
    {
      id: "cmd:new",
      label: "New chat",
      hint: "⌘N",
      group: "Commands",
      icon: "command",
      run: () => void newChat(store.activeChat()?.projectId ?? null),
    },
    {
      id: "cmd:terminal",
      label: "Toggle terminal",
      hint: "⌃`",
      group: "Commands",
      icon: "terminal",
      run: () => togglePane("terminal"),
    },
    {
      id: "cmd:files",
      label: "Browse files",
      hint: "⌘⇧F",
      group: "Commands",
      icon: "folder",
      run: () => togglePane("files"),
    },
    {
      id: "cmd:analysis",
      label: "Architecture map",
      hint: "⌘⇧G",
      group: "Commands",
      icon: "command",
      run: () => togglePane("analysis"),
    },
    {
      id: "cmd:graph",
      label: "Execution graph",
      hint: "⌘⇧E",
      group: "Commands",
      icon: "command",
      run: () => togglePane("graph"),
    },
    {
      id: "cmd:compact",
      label: "Compact this chat's context",
      group: "Commands",
      icon: "command",
      run: () => {
        const id = store.activeChatId();
        if (!id) return;
        void import("~/lib/ipc").then(({ api }) =>
          import("~/lib/session").then(({ getSession }) =>
            api
              .compactChat(id)
              .then((o) =>
                getSession(id)
                  .load()
                  .then(() => o),
              )
              .then((o) =>
                import("~/lib/store").then(({ toast }) => toast("ok", `Compacted ${o.removed} message(s)`)),
              )
              .catch((e) => import("~/lib/store").then(({ toast }) => toast("warn", errText(e)))),
          ),
        );
      },
    },
    {
      id: "cmd:maximize",
      label: "Maximize side pane",
      hint: "⌘⇧M",
      group: "Commands",
      icon: "command",
      run: () => toggleMaximizedPane(),
    },
    {
      id: "cmd:browser",
      label: "Open browser",
      hint: "⌘⇧B",
      group: "Commands",
      icon: "command",
      run: () => toggleBrowser(),
    },
    {
      id: "cmd:diff",
      label: "Show latest diff",
      hint: "⌘⇧D",
      group: "Commands",
      icon: "command",
      run: () => void showLatestDiff(),
    },
    {
      id: "cmd:changes",
      label: "Show changed files",
      group: "Commands",
      icon: "command",
      run: () => togglePane("changes"),
    },
    {
      id: "cmd:settings",
      label: "Settings",
      hint: "⌘,",
      group: "Commands",
      icon: "settings",
      run: () => store.setSettingsOpen(true),
    },
    {
      id: "cmd:theme",
      label: "Cycle theme",
      hint: "⌘⇧L",
      group: "Commands",
      icon: "command",
      run: cycleTheme,
    },
    {
      id: "cmd:rail",
      label: "Toggle sidebar",
      hint: "⌘B",
      group: "Commands",
      icon: "command",
      run: () => store.setRailCollapsed(!store.railCollapsed()),
    },
    {
      id: "cmd:folder",
      label: "Set working folder for this chat",
      group: "Commands",
      icon: "folder",
      run: () =>
        void (async () => {
          const id = store.activeChatId();
          if (!id) return;
          const dir = await pickDirectory("Working folder for this chat");
          if (dir) {
            await api.updateChat({ id, cwd: dir });
            void store.refreshChats();
          }
        })(),
    },
    {
      id: "cmd:models",
      label: "Refresh model catalogue",
      group: "Commands",
      icon: "command",
      run: () => void store.refreshModels(true),
    },
  ];

  const items = createMemo<Item[]>(() => {
    const q = query().trim();

    const chatItems: Item[] = store.chats().map((c) => ({
      id: `chat:${c.id}`,
      label: c.title,
      hint: store.projects().find((p) => p.id === c.projectId)?.name,
      group: "Chats",
      icon: "chat" as const,
      run: () => store.setActiveChatId(c.id),
    }));

    const projectItems: Item[] = store.projects().map((p) => ({
      id: `project:${p.id}`,
      label: p.name,
      hint: p.rootPath ?? undefined,
      group: "Projects",
      icon: "folder" as const,
      run: () => {
        store.setExpandedProjects((prev) => new Set(prev).add(p.id));
        const first = store.chats().find((c) => c.projectId === p.id);
        if (first) store.setActiveChatId(first.id);
        else void newChat(p.id);
      },
    }));

    const searchItems: Item[] = hits().map((h) => ({
      id: `hit:${h.messageId}`,
      label: h.snippet.replace(/\s+/g, " ").trim().slice(0, 110),
      hint: h.chatTitle,
      group: "In messages",
      icon: "search" as const,
      run: () => store.setActiveChatId(h.chatId),
    }));

    const pool = [...commands(), ...chatItems, ...projectItems];
    if (!q) return [...commands().slice(0, 6), ...chatItems.slice(0, 8)];

    const ranked = pool
      .map((it) => ({ it, s: Math.max(score(it.label, q), score(it.hint ?? "", q) - 40) }))
      .filter((r) => r.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 24)
      .map((r) => r.it);

    return [...ranked, ...searchItems];
  });

  createEffect(on(items, () => setCursor(0)));

  function activate(i: number) {
    const item = items()[i];
    if (!item) return;
    store.setPaletteOpen(false);
    item.run();
  }

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      store.setPaletteOpen(false);
      return;
    }
    if (e.key === "ArrowDown" || (e.key === "n" && e.ctrlKey)) {
      e.preventDefault();
      setCursor((c) => Math.min(items().length - 1, c + 1));
      scrollCursorIntoView();
    }
    if (e.key === "ArrowUp" || (e.key === "p" && e.ctrlKey)) {
      e.preventDefault();
      setCursor((c) => Math.max(0, c - 1));
      scrollCursorIntoView();
    }
    if (e.key === "Enter") {
      e.preventDefault();
      activate(cursor());
    }
  };

  function scrollCursorIntoView() {
    queueMicrotask(() => {
      listEl?.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView({ block: "nearest" });
    });
  }

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: modal backdrop; Escape closes it and every item inside is focusable
    <div
      class="scrim"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) store.setPaletteOpen(false);
      }}
    >
      <div class="palette" role="dialog" aria-label="Command palette">
        <div class="palette-input">
          <Search size={15} />
          <input
            ref={input}
            placeholder="Search chats, run a command, find a message…"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
            onKeyDown={onKeyDown}
            spellcheck={false}
          />
          <kbd>Esc</kbd>
        </div>

        <div class="palette-list" ref={listEl}>
          <For each={items()} fallback={<div class="palette-empty muted">Nothing matches.</div>}>
            {(item, i) => {
              const showGroup = () => {
                const prev = items()[i() - 1]?.group;
                return prev !== item.group;
              };
              return (
                <>
                  <Show when={showGroup()}>
                    <div class="palette-group">{item.group}</div>
                  </Show>
                  <button
                    type="button"
                    class="palette-item"
                    data-cursor={i() === cursor()}
                    onMouseEnter={() => setCursor(i())}
                    onClick={() => activate(i())}
                  >
                    <PaletteIcon kind={item.icon} />
                    <span class="truncate">{item.label}</span>
                    <Show when={item.hint}>
                      {(h) => <span class="palette-hint muted truncate">{h()}</span>}
                    </Show>
                  </button>
                </>
              );
            }}
          </For>
        </div>
      </div>
    </div>
  );
}

function PaletteIcon(props: { kind: Item["icon"] }) {
  switch (props.kind) {
    case "chat":
      return <ChatIcon size={14} />;
    case "folder":
      return <Folder size={14} />;
    case "settings":
      return <SettingsIcon size={14} />;
    case "terminal":
      return <Terminal size={14} />;
    case "search":
      return <Search size={14} />;
    default:
      return <CommandIcon size={14} />;
  }
}
