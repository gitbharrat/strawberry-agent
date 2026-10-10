// Activity: what the harness is doing right now.
//
// Three ledgers in one pane — background shell sessions, the active run's tool
// calls, and any chats with live runs. Sessions poll on a slow tick only while
// the pane is visible, so an idle app makes no IPC traffic.

import { createEffect, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { Close, Refresh, Server, Terminal } from "./Icons";
import { ToolRow } from "./ToolRow";
import { api } from "~/lib/ipc";
import { getSession } from "~/lib/session";
import { store, toast } from "~/lib/store";
import { formatBytes } from "~/lib/actions";
import { errText } from "~/lib/err";

export function ActivityPane() {
  const [tick, setTick] = createSignal(0);
  const [shells, { refetch }] = createResource(tick, () => api.bgShells());

  createEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 3000);
    onCleanup(() => clearInterval(id));
  });

  const runningChats = () => [...store.runningChats()];
  const activeSession = () => {
    const id = store.activeChatId();
    return id ? getSession(id) : null;
  };
  const liveTools = () => {
    const s = activeSession();
    if (!s) return [];
    return s.toolsFor(s.live.currentMessageId ?? "").concat(s.orphanTools());
  };

  async function kill(id: string) {
    try {
      await api.bgShellKill(id);
      void refetch();
    } catch (e) {
      toast("error", errText(e));
    }
  }

  return (
    <div class="activity">
      {/* No header bar: the tab names the pane, and the refresh control lives
          in the first section head so the content gets the whole height. */}
      <div class="activity-scroll">
        <section class="activity-section">
          <header class="rail-section-head">
            <span>Background sessions</span>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="Refresh"
              onClick={() => void refetch()}
            >
              <Refresh size={12} />
            </button>
          </header>
          <For
            each={shells() ?? []}
            fallback={<div class="activity-empty muted">Nothing running in the background.</div>}
          >
            {(s) => (
              <div class="activity-row">
                <Terminal size={12} />
                <div class="activity-main">
                  <code class="truncate" title={s.command}>
                    {s.command}
                  </code>
                  <span class="muted tabular">
                    {s.alive ? "running" : "exited"} · {formatBytes(s.bufferedBytes)} buffered
                  </span>
                </div>
                <button
                  type="button"
                  class="btn"
                  data-size="icon-sm"
                  data-variant="danger"
                  title="Kill session"
                  onClick={() => void kill(s.id)}
                >
                  <Close size={11} />
                </button>
              </div>
            )}
          </For>
        </section>

        <section class="activity-section">
          <header class="rail-section-head">
            <span>Current run</span>
          </header>
          <Show
            when={liveTools().length > 0}
            fallback={<div class="activity-empty muted">No tools in flight for this chat.</div>}
          >
            <For each={liveTools()}>{(t) => <ToolRow tool={t} />}</For>
          </Show>
        </section>

        <section class="activity-section">
          <header class="rail-section-head">
            <span>Running chats</span>
          </header>
          <For each={runningChats()} fallback={<div class="activity-empty muted">No live runs.</div>}>
            {(id) => {
              const chat = () => store.chats().find((c) => c.id === id);
              return (
                <button
                  type="button"
                  class="activity-row activity-link"
                  onClick={() => store.setActiveChatId(id)}
                >
                  <Server size={12} />
                  <span class="truncate">{chat()?.title ?? id}</span>
                  <span class="row-live" />
                </button>
              );
            }}
          </For>
        </section>
      </div>
    </div>
  );
}
