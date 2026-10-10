// The left rail: projects, and the chats inside them.
//
// One list, one level of nesting, no tabs and no secondary sidebar. Row
// actions stay hidden until hover so the default state is quiet — the rail is
// navigation, not a control panel.

import { createMemo, createSignal, For, Show } from "solid-js";
import {
  Chat as ChatIcon,
  Chevron,
  Folder,
  Plus,
  Search,
  Settings as SettingsIcon,
  Sidebar,
  Trash,
} from "./Icons";
import { Artifacts } from "./Artifacts";
import { store } from "~/lib/store";
import { deleteChat, newChat, newProject, pickDirectory } from "~/lib/actions";
import type { Chat } from "~/lib/types";

export function Rail() {
  const [creatingProject, setCreatingProject] = createSignal(false);
  const [projectName, setProjectName] = createSignal("");

  // A chat earns its place in history by having content. Empty ones show only
  // while they are open as a tab or active, so pressing ⌘N does not slowly
  // fill the rail with identical "New chat" rows.
  const visible = (c: import("~/lib/types").Chat) =>
    c.messageCount > 0 ||
    store.runningChats().has(c.id) ||
    store.openChats().includes(c.id) ||
    store.activeChatId() === c.id;

  const looseChats = createMemo(() => store.chats().filter((c) => !c.projectId && visible(c)));
  const chatsFor = (projectId: string) =>
    store.chats().filter((c) => c.projectId === projectId && visible(c));

  async function commitProject() {
    const name = projectName().trim();
    setCreatingProject(false);
    setProjectName("");
    if (!name) return;
    const root = await pickDirectory(`Working folder for "${name}" (optional)`);
    await newProject(name, root ?? undefined);
  }

  return (
    <nav class="rail">
      <div class="rail-head" data-tauri-drag-region>
        <div class="spacer" />
        <button
          type="button"
          class="btn"
          data-size="icon"
          title="Hide sidebar  ⌘B"
          onClick={() => store.setRailCollapsed(true)}
        >
          <Sidebar size={15} />
        </button>
      </div>

      <button type="button" class="rail-search" onClick={() => store.setPaletteOpen(true)}>
        <Search size={13} />
        <span>Search</span>
        <kbd>⌘K</kbd>
      </button>

      <div class="rail-scroll">
        <section class="rail-section">
          <header class="rail-section-head">
            <span>Projects</span>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="New project"
              onClick={() => setCreatingProject(true)}
            >
              <Plus size={12} />
            </button>
          </header>

          <Show when={creatingProject()}>
            <input
              class="input rail-new"
              autofocus
              placeholder="Project name"
              value={projectName()}
              onInput={(e) => setProjectName(e.currentTarget.value)}
              onBlur={() => void commitProject()}
              onKeyDown={(e) => {
                if (e.key === "Enter") void commitProject();
                if (e.key === "Escape") {
                  setCreatingProject(false);
                  setProjectName("");
                }
              }}
            />
          </Show>

          <Show when={store.projects().length === 0 && !creatingProject()}>
            <div class="rail-hint muted">Group related chats and give them a folder.</div>
          </Show>

          <For each={store.projects()}>
            {(p) => {
              const open = () => store.expandedProjects().has(p.id);
              return (
                <>
                  <div class="row" data-open={open()}>
                    <button
                      type="button"
                      class="row-main"
                      onClick={() => store.toggleProject(p.id)}
                      title={p.rootPath ?? p.name}
                    >
                      <span class="row-chevron">
                        <Chevron size={11} />
                      </span>
                      <Folder size={13} />
                      <span class="row-label">{p.name}</span>
                    </button>
                    <span class="row-actions">
                      <button
                        type="button"
                        class="btn"
                        data-size="icon-sm"
                        title="New chat in this project"
                        onClick={() => void newChat(p.id)}
                      >
                        <Plus size={12} />
                      </button>
                    </span>
                    <Show when={!open()}>
                      <span class="row-meta">{p.chatCount}</span>
                    </Show>
                  </div>
                  <Show when={open()}>
                    <div class="chat-children">
                      <For
                        each={chatsFor(p.id)}
                        fallback={
                          <div class="row muted" style={{ "font-size": "var(--fs-sm)" }}>
                            No chats yet
                          </div>
                        }
                      >
                        {(c) => <ChatRow chat={c} />}
                      </For>
                    </div>
                  </Show>
                </>
              );
            }}
          </For>
        </section>

        <section class="rail-section">
          <header class="rail-section-head">
            <span>Chats</span>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="New chat  ⌘N"
              onClick={() => void newChat(null)}
            >
              <Plus size={12} />
            </button>
          </header>
          <For each={looseChats()} fallback={<div class="rail-hint muted">⌘N starts one.</div>}>
            {(c) => <ChatRow chat={c} />}
          </For>
        </section>

        <Artifacts />
      </div>

      <div class="rail-foot">
        <div class="spacer" />
        <button
          type="button"
          class="btn"
          data-size="icon"
          title="Settings  ⌘,"
          onClick={() => store.setSettingsOpen(true)}
        >
          <SettingsIcon size={15} />
        </button>
      </div>
    </nav>
  );
}

function ChatRow(props: { chat: Chat }) {
  const active = () => store.activeChatId() === props.chat.id;
  const running = () => store.runningChats().has(props.chat.id);

  return (
    <div class="row" data-active={active()}>
      <button
        type="button"
        class="row-main"
        onClick={() => store.setActiveChatId(props.chat.id)}
        title={props.chat.title}
      >
        <Show when={running()} fallback={<ChatIcon size={13} />}>
          <span class="row-live" />
        </Show>
        <span class="row-label">{props.chat.title}</span>
      </button>
      <span class="row-actions">
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Delete chat"
          onClick={() => void deleteChat(props.chat.id)}
        >
          <Trash size={12} />
        </button>
      </span>
    </div>
  );
}
