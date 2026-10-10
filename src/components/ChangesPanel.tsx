// "What did this chat change?"
//
// The answer every agent user wants and few tools give: one row per file, with
// the diff a click away and an undo button next to it. Reverts are per-file or
// all at once, and a file edited outside the app after the change is flagged
// before anything is overwritten.

import { createEffect, createResource, For, Show } from "solid-js";
import { File as FileIcon, Refresh, Undo } from "./Icons";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { formatBytes, formatWhen, openPath } from "~/lib/actions";
import { getSession } from "~/lib/session";
import type { ChangedFile } from "~/lib/types";
import { errText } from "~/lib/err";

export function ChangesPanel() {
  const chatId = () => store.activeChatId();
  const [changes, { refetch }] = createResource(chatId, (id) => api.chatChanges(id));

  // Refresh whenever the active run settles.
  createEffect(() => {
    const id = chatId();
    if (!id) return;
    const status = getSession(id).live.status;
    if (status === "idle") void refetch();
  });

  const pending = () => (changes() ?? []).filter((c) => !c.reverted);

  async function revert(c: ChangedFile, force = false) {
    try {
      await api.revertChange(c.latestChangeId, force);
      toast("ok", `Reverted ${c.name}`);
      void refetch();
    } catch (e) {
      const msg = errText(e);
      if (msg.includes("changed since")) {
        toast("warn", `${c.name} changed since that edit`, {
          label: "Revert anyway",
          run: () => void revert(c, true),
        });
      } else {
        toast("error", msg);
      }
    }
  }

  async function revertAll() {
    const id = chatId();
    if (!id) return;
    try {
      const results = await api.revertAll(id, false);
      toast("ok", `Reverted ${results.length} file${results.length === 1 ? "" : "s"}`);
      void refetch();
    } catch (e) {
      toast("error", errText(e));
    }
  }

  return (
    <div class="changes">
      <div class="code-head">
        <Show when={pending().length > 0}>
          <span class="chip" data-tone="accent" title="Files with unreverted changes">
            {pending().length} pending
          </span>
        </Show>
        <div class="spacer" />
        <button type="button" class="btn" data-size="icon-sm" title="Refresh" onClick={() => void refetch()}>
          <Refresh size={12} />
        </button>
        <Show when={pending().length > 1}>
          <button
            type="button"
            class="btn"
            data-size="sm"
            data-variant="danger"
            onClick={() => void revertAll()}
          >
            <Undo size={12} /> Revert all
          </button>
        </Show>
      </div>

      <div class="changes-list">
        <For
          each={changes() ?? []}
          fallback={
            <div class="empty">
              <p>
                Nothing changed yet. Files the agent creates or edits show up here, each with a diff and an
                undo.
              </p>
            </div>
          }
        >
          {(c) => (
            <div class="change-row" data-reverted={c.reverted}>
              <button
                type="button"
                class="change-main"
                onClick={() => store.openPane({ kind: "diff", changeId: c.latestChangeId })}
              >
                <FileIcon size={13} />
                <span class="change-name truncate" title={c.path}>
                  {c.name}
                </span>
                <span class="change-op chip" data-tone={c.op === "create" ? "ok" : undefined}>
                  {c.reverted ? "reverted" : c.op}
                </span>
                <Show when={c.origin === "manual"}>
                  <span class="chip">yours</span>
                </Show>
                <Show when={c.edits > 1}>
                  <span class="muted tabular">{c.edits} edits</span>
                </Show>
                <div class="spacer" />
                <span class="muted tabular">
                  {formatBytes(c.bytesBefore)} → {formatBytes(c.bytesAfter)}
                </span>
                <span class="muted tabular">{formatWhen(c.updatedAt)}</span>
              </button>
              <div class="change-actions">
                <button
                  type="button"
                  class="btn"
                  data-size="icon-sm"
                  title="Open file"
                  onClick={() => void openPath(c.path)}
                >
                  <FileIcon size={12} />
                </button>
                <Show when={!c.reverted}>
                  <button
                    type="button"
                    class="btn"
                    data-size="icon-sm"
                    title="Revert this file"
                    onClick={() => void revert(c)}
                  >
                    <Undo size={12} />
                  </button>
                </Show>
              </div>
              <div class="change-path muted truncate">{c.path}</div>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
