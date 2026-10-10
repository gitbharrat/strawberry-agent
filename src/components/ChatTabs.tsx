// Open chats as tabs, and the workspace actions on the same line.
//
// The rail is the whole library; these are the few chats you are actually
// working in. Middle-click closes, ⌘1–9 jumps, and a running chat shows the
// same pulse the rail uses. The right side holds every pane action for the
// active chat — one 32px bar instead of a tab row plus a near-empty toolbar.

import { createEffect, createResource, For, Show } from "solid-js";
import {
  Chat as ChatIcon,
  Close,
  Diff,
  Folder,
  Globe,
  Layers,
  Network,
  Play,
  Plus,
  Sidebar,
  Sparkle,
  NodeGraph,
  Split,
  Terminal,
} from "./Icons";
import { store } from "~/lib/store";
import { newChat, pickDirectory, toggleBrowser, togglePane, toggleSidePane } from "~/lib/actions";
import { getSession } from "~/lib/session";
import { api } from "~/lib/ipc";

export function ChatTabs() {
  const tabs = () => store.openChats();
  const activeId = () => store.activeChatId();
  const activeChat = () => store.activeChat();
  const session = () => {
    const id = activeId();
    return id ? getSession(id) : null;
  };

  const [changes, { refetch }] = createResource(activeId, (id) => api.chatChanges(id));
  // Re-count changed files whenever the active run settles.
  createEffect(() => {
    if (session()?.live.status === "idle") void refetch();
  });
  const pending = () => (changes() ?? []).filter((c) => !c.reverted).length;

  async function chooseFolder() {
    const id = activeId();
    if (!id) return;
    const dir = await pickDirectory("Working folder for this chat");
    if (!dir) return;
    await api.updateChat({ id, cwd: dir });
    void store.refreshChats();
  }

  // With the rail collapsed the strip always renders: the expand control
  // lives inline as its first element, where it cannot overlap a tab.
  return (
    <Show when={tabs().length > 0 || store.railCollapsed()}>
      <div class="chat-tabs" data-tauri-drag-region>
        <Show when={store.railCollapsed()}>
          <button
            type="button"
            class="btn chat-tabs-expand"
            data-size="icon-sm"
            title="Show sidebar  ⌘B"
            onClick={() => store.setRailCollapsed(false)}
          >
            <Sidebar size={13} />
          </button>
        </Show>
        <For each={tabs()}>
          {(id, i) => {
            const chat = () => store.chats().find((c) => c.id === id);
            const active = () => store.activeChatId() === id;
            const running = () => store.runningChats().has(id);
            return (
              <Show when={chat()}>
                {(c) => (
                  <div class="chat-tab" data-active={active()}>
                    <button
                      type="button"
                      class="chat-tab-main"
                      onClick={() => store.setActiveChatId(id)}
                      onAuxClick={(e) => {
                        if (e.button === 1) store.closeChatTab(id);
                      }}
                      title={`${c().title}${i() < 9 ? `  ⌘${i() + 1}` : ""}`}
                    >
                      <Show when={running()} fallback={<ChatIcon size={11} />}>
                        <span class="row-live" />
                      </Show>
                      <span class="truncate">{c().title}</span>
                    </button>
                    <button
                      type="button"
                      class="chat-tab-close"
                      aria-label={`Close ${c().title}`}
                      onClick={() => store.closeChatTab(id)}
                    >
                      <Close size={9} />
                    </button>
                  </div>
                )}
              </Show>
            );
          }}
        </For>
        <button
          type="button"
          class="btn chat-tab-new"
          data-size="icon-sm"
          title="New chat  ⌘N"
          onClick={() => void newChat(store.activeChat()?.projectId ?? null)}
        >
          <Plus size={12} />
        </button>

        <div class="spacer" />

        <Show when={activeId()}>
          <Show when={activeChat() && !activeChat()?.cwd}>
            <button
              type="button"
              class="btn"
              data-size="sm"
              onClick={() => void chooseFolder()}
              title="Give the agent a folder to work in"
            >
              <Folder size={13} /> Set folder
            </button>
          </Show>
          <Show when={session()?.plan()}>
            <button
              type="button"
              class="btn"
              data-size="sm"
              data-on={store.activeTab()?.surface.kind === "plan"}
              title="The agent's plan for this chat  ⌘⇧P"
              onClick={() => store.togglePaneSurface({ kind: "plan" })}
            >
              Plan
            </button>
          </Show>
          <Show when={pending() > 0}>
            <button
              type="button"
              class="btn"
              data-size="sm"
              data-on={store.activeTab()?.surface.kind === "changes"}
              onClick={() => togglePane("changes")}
              title="Files this chat changed  ⌘⇧C"
            >
              <Diff size={13} /> {pending()}
            </button>
          </Show>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.activeTab()?.surface.kind === "files"}
            title="Files  ⌘⇧F"
            onClick={() => togglePane("files")}
          >
            <Layers size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.activeTab()?.surface.kind === "analysis"}
            title="Architecture map  ⌘⇧G"
            onClick={() => togglePane("analysis")}
          >
            <Network size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.activeTab()?.surface.kind === "graph"}
            title="Execution graph  ⌘⇧E"
            onClick={() => togglePane("graph")}
          >
            <NodeGraph size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.activeTab()?.surface.kind === "browser"}
            title="Browser  ⌘⇧B"
            onClick={() => toggleBrowser()}
          >
            <Globe size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.activeTab()?.surface.kind === "activity"}
            title="Activity — sessions, tools, agents  ⌘⇧R"
            onClick={() => store.togglePaneSurface({ kind: "activity" })}
          >
            <Play size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.sideChatOpen()}
            title="Side chat  ⌘⇧A"
            onClick={() => store.setSideChatOpen(!store.sideChatOpen())}
          >
            <Sparkle size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            data-on={store.activeTab()?.surface.kind === "terminal"}
            title="Terminal  ⌃`"
            onClick={() => togglePane("terminal")}
          >
            <Terminal size={14} />
          </button>
          <button
            type="button"
            class="btn"
            data-size="icon"
            title="Toggle side pane  ⌘J"
            onClick={toggleSidePane}
          >
            <Split size={14} />
          </button>
        </Show>
      </div>
    </Show>
  );
}
