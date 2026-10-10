// Diff viewer.
//
// Unified by default because it reads better in a side pane; split is one
// click away for wide changes. Both views share the same computed diff, and
// syntax highlighting is applied per line so a diff of code still reads as code.

import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { Close, Split, Undo } from "./Icons";
import { diffLines, diffStats, toHunks, type DiffLine } from "~/lib/diff";
import { highlight } from "~/lib/highlight";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { errText } from "~/lib/err";

export function DiffView(props: { changeId: string; tabId: string }) {
  const [split, setSplit] = createSignal(false);
  const [full, setFull] = createSignal(false);
  const [diff, { refetch }] = createResource(
    () => props.changeId,
    (id) => api.changeDiff(id),
  );

  const lines = createMemo(() => {
    const d = diff();
    return d ? diffLines(d.before, d.after) : [];
  });
  const stats = createMemo(() => diffStats(lines()));
  const hunks = createMemo(() =>
    full() ? [{ oldStart: 1, newStart: 1, lines: lines() }] : toHunks(lines(), 3),
  );

  async function revert(force = false) {
    try {
      const r = await api.revertChange(props.changeId, force);
      toast("ok", `Reverted ${r.path.split("/").pop()}`);
      void refetch();
    } catch (e) {
      const msg = errText(e);
      if (msg.includes("changed since")) {
        toast("warn", "File changed since this edit", {
          label: "Revert anyway",
          run: () => void revert(true),
        });
      } else {
        toast("error", msg);
      }
    }
  }

  return (
    <div class="diff">
      <div class="code-head">
        <span class="truncate" title={diff()?.path}>
          {diff()?.path.split("/").pop() ?? "Diff"}
        </span>
        <span class="diff-stats tabular">
          <span class="diff-add">+{stats().added}</span>
          <span class="diff-del">−{stats().removed}</span>
        </span>
        <Show when={diff()?.reverted}>
          <span class="chip">reverted</span>
        </Show>
        <div class="spacer" />
        <button type="button" class="btn" data-size="sm" onClick={() => setFull(!full())}>
          {full() ? "Changes only" : "Whole file"}
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title={split() ? "Unified" : "Side by side"}
          onClick={() => setSplit(!split())}
        >
          <Split size={12} />
        </button>
        <Show when={!diff()?.reverted}>
          <button
            type="button"
            class="btn"
            data-size="sm"
            data-variant="danger"
            onClick={() => void revert()}
          >
            <Undo size={12} /> Revert
          </button>
        </Show>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close"
          onClick={() => store.closeTab(props.tabId)}
        >
          <Close size={12} />
        </button>
      </div>

      <div class="diff-body selectable">
        <Show
          when={diff()}
          fallback={
            <div class="empty">
              <p>Loading…</p>
            </div>
          }
        >
          <Show
            when={lines().length > 0}
            fallback={
              <div class="empty">
                <p>No differences.</p>
              </div>
            }
          >
            <For each={hunks()}>
              {(hunk, i) => (
                <>
                  <Show when={!full() && i() >= 0}>
                    <div class="diff-hunk-head">
                      @@ −{hunk.oldStart} +{hunk.newStart} @@
                    </div>
                  </Show>
                  <Show when={split()} fallback={<Unified lines={hunk.lines} lang={diff()!.language} />}>
                    <SideBySide lines={hunk.lines} lang={diff()!.language} />
                  </Show>
                </>
              )}
            </For>
          </Show>
        </Show>
      </div>
    </div>
  );
}

function Unified(props: { lines: DiffLine[]; lang: string }) {
  return (
    <div class="diff-unified">
      <For each={props.lines}>
        {(l) => (
          <div class="diff-line" data-op={l.op}>
            <span class="diff-no tabular">{l.oldNo ?? ""}</span>
            <span class="diff-no tabular">{l.newNo ?? ""}</span>
            <span class="diff-sign">{l.op === "insert" ? "+" : l.op === "delete" ? "−" : " "}</span>
            <span class="diff-text" innerHTML={highlight(l.text, props.lang) || "&nbsp;"} />
          </div>
        )}
      </For>
    </div>
  );
}

/**
 * Pairs deletes with the inserts that replaced them so changed lines sit
 * opposite each other rather than drifting apart down the column.
 */
function SideBySide(props: { lines: DiffLine[]; lang: string }) {
  const rows = createMemo(() => {
    const out: { left: DiffLine | null; right: DiffLine | null }[] = [];
    let i = 0;
    const ls = props.lines;
    while (i < ls.length) {
      const l = ls[i]!;
      if (l.op === "equal") {
        out.push({ left: l, right: l });
        i++;
        continue;
      }

      const dels: DiffLine[] = [];
      const ins: DiffLine[] = [];
      while (i < ls.length && ls[i]!.op === "delete") {
        dels.push(ls[i]!);
        i++;
      }
      while (i < ls.length && ls[i]!.op === "insert") {
        ins.push(ls[i]!);
        i++;
      }
      for (let k = 0; k < Math.max(dels.length, ins.length); k++) {
        out.push({ left: dels[k] ?? null, right: ins[k] ?? null });
      }
    }
    return out;
  });

  return (
    <div class="diff-split">
      <For each={rows()}>
        {(r) => (
          <div class="diff-srow">
            <span class="diff-no tabular">{r.left?.oldNo ?? ""}</span>
            <span
              class="diff-scell"
              data-op={r.left?.op ?? "blank"}
              innerHTML={r.left ? highlight(r.left.text, props.lang) || "&nbsp;" : ""}
            />
            <span class="diff-no tabular">{r.right?.newNo ?? ""}</span>
            <span
              class="diff-scell"
              data-op={r.right?.op ?? "blank"}
              innerHTML={r.right ? highlight(r.right.text, props.lang) || "&nbsp;" : ""}
            />
          </div>
        )}
      </For>
    </div>
  );
}
