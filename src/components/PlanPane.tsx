// The plan, as the model maintains it.
//
// One document per chat, replaced whole on each update, rendered through the
// same Markdown pipeline as the transcript — so checkboxes, mermaid flowcharts
// and LaTeX come for free. A progress line up top summarises the checkboxes
// without needing to read them.

import { createMemo, Show } from "solid-js";
import { Markdown } from "./Markdown";
import { Close, Refresh } from "./Icons";
import { getSession } from "~/lib/session";
import { store } from "~/lib/store";
import { formatWhen } from "~/lib/actions";

export function PlanPane(props: { tabId: string }) {
  const session = () => {
    const id = store.activeChatId();
    return id ? getSession(id) : null;
  };
  const plan = () => session()?.plan() ?? null;

  const progress = createMemo(() => {
    const md = plan()?.markdown ?? "";
    const total = (md.match(/^\s*[-*]\s+\[[ xX]\]/gm) ?? []).length;
    const done = (md.match(/^\s*[-*]\s+\[[xX]\]/gm) ?? []).length;
    return total > 0 ? { done, total, pct: Math.round((done / total) * 100) } : null;
  });

  return (
    <div class="plan">
      <div class="code-head">
        <span class="truncate">{plan()?.title ?? "Plan"}</span>
        <Show when={progress()}>
          {(p) => (
            <span class="chip tabular" title={`${p().done} of ${p().total} steps done`}>
              {p().done}/{p().total}
            </span>
          )}
        </Show>
        <div class="spacer" />
        <Show when={plan()}>{(doc) => <span class="muted tabular">{formatWhen(doc().updatedAt)}</span>}</Show>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Reload"
          onClick={() => void session()?.load()}
        >
          <Refresh size={12} />
        </button>
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

      <Show when={progress()}>
        {(p) => (
          <div class="plan-progress" role="presentation">
            <span class="plan-progress-fill" style={{ width: `${p().pct}%` }} />
          </div>
        )}
      </Show>

      <div class="plan-body selectable">
        <Show
          when={plan()}
          fallback={
            <div class="empty">
              <p>
                No plan yet. The agent writes one here when it starts multi-step work — ask it to "make a plan
                first" to see one.
              </p>
            </div>
          }
        >
          {(doc) => <Markdown text={doc().markdown} />}
        </Show>
      </div>
    </div>
  );
}
