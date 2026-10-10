// Artifacts: files the agent created that still exist on disk.
//
// Edits belong in the per-chat changes panel; this is the durable output — the
// things you would hand to someone else. Clicking one opens it in whichever
// surface suits its type.

import { createEffect, createResource, For, Show } from "solid-js";
import { Code, File as FileIcon, Globe, Image as ImageIcon, Refresh } from "./Icons";
import { api } from "~/lib/ipc";
import { store } from "~/lib/store";
import { formatWhen, openPath } from "~/lib/actions";
import { getSession } from "~/lib/session";

const IMAGE = /\.(png|jpe?g|gif|webp|avif|svg)$/i;
const WEB = /\.(html?|md|markdown)$/i;

export function Artifacts() {
  // createResource treats a null/undefined source as "not ready" and never
  // fetches, so an explicit sentinel stands in for "no project scope".
  const ALL = "\u0000all";
  const scope = () => store.activeProject()?.id ?? ALL;
  const [items, { refetch }] = createResource(scope, (id) => api.artifacts(id === ALL ? null : id, 40));

  // New artifacts appear as soon as the run that made them finishes.
  createEffect(() => {
    const chatId = store.activeChatId();
    if (!chatId) return;
    if (getSession(chatId).live.status === "idle") void refetch();
  });

  return (
    <section class="rail-section">
      <header class="rail-section-head">
        <span>Artifacts</span>
        <button type="button" class="btn" data-size="icon-sm" title="Refresh" onClick={() => void refetch()}>
          <Refresh size={12} />
        </button>
      </header>

      <Show
        when={(items() ?? []).length > 0}
        fallback={<div class="rail-hint muted">Files the agent creates show up here.</div>}
      >
        <For each={items()}>
          {(a) => (
            <button type="button" class="row" onClick={() => void openPath(a.path)} title={a.path}>
              <Show
                when={IMAGE.test(a.name)}
                fallback={
                  <Show
                    when={WEB.test(a.name)}
                    fallback={
                      <Show when={/\.[a-z]+$/i.test(a.name)} fallback={<FileIcon size={13} />}>
                        <Code size={13} />
                      </Show>
                    }
                  >
                    <Globe size={13} />
                  </Show>
                }
              >
                <ImageIcon size={13} />
              </Show>
              <span class="row-label">{a.name}</span>
              <span class="row-meta">{formatWhen(a.createdAt)}</span>
            </button>
          )}
        </For>
      </Show>
    </section>
  );
}
