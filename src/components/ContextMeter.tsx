// Context-window meter.
//
// Answers "how full is this conversation?" at a glance. The prompt size of the
// most recent request is the context size — exact, straight from the
// provider's usage frame — and the window comes from the model catalogue.
// Between runs it falls back to the last assistant message that carried usage.
//
// Clicking the meter answers the follow-up question — full *of what?* — with a
// breakdown measured from the real request parts: the actual system prompt,
// the architecture block inside it, the tool schemas, and the history exactly
// as it would be encoded right now.

import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { formatTokens } from "~/lib/actions";
import { api } from "~/lib/ipc";
import type { Session } from "~/lib/session";
import { store, toast } from "~/lib/store";
import type { ContextBreakdown, Usage } from "~/lib/types";
import { errText } from "~/lib/err";

export function ContextMeter(props: { session: Session; chatId: string }) {
  const modelId = () =>
    store.chats().find((c) => c.id === props.chatId)?.model ?? store.settings()?.defaultModel ?? "";

  const windowSize = () => {
    const m = store.models().find((x) => x.id === modelId());
    return m?.contextLength ?? m?.topProvider?.contextLength ?? 0;
  };

  const used = createMemo(() => {
    const live = props.session.live.contextTokens;
    if (live > 0) return live;
    // Idle: read the last billed request out of the transcript.
    for (let i = props.session.messages.length - 1; i >= 0; i--) {
      const raw = props.session.messages[i]?.usage;
      if (!raw) continue;
      try {
        const u = JSON.parse(raw) as Usage;
        if (u.prompt_tokens > 0) return u.prompt_tokens + u.completion_tokens;
      } catch {
        /* skip malformed rows */
      }
    }
    return 0;
  });

  const pct = () => {
    const w = windowSize();
    return w > 0 ? Math.min(100, (used() / w) * 100) : 0;
  };

  const tone = () => (pct() >= 90 ? "danger" : pct() >= 70 ? "warn" : "ok");

  const [compacting, setCompacting] = createSignal(false);
  async function compact() {
    if (compacting()) return;
    setCompacting(true);
    try {
      const o = await api.compactChat(props.chatId);
      toast("ok", `Compacted: ${o.removed} message(s) folded into a summary, ${o.kept} kept`);
      await props.session.load();
    } catch (e) {
      toast("warn", errText(e));
    } finally {
      setCompacting(false);
    }
  }

  // ---- breakdown popover
  const [open, setOpen] = createSignal(false);
  const [detail, setDetail] = createSignal<ContextBreakdown | null>(null);
  let wrap!: HTMLSpanElement;

  function toggle() {
    if (open()) {
      setOpen(false);
      return;
    }
    setOpen(true);
    setDetail(null);
    void api
      .contextBreakdown(props.chatId)
      .then(setDetail)
      .catch((e) => {
        toast("warn", errText(e));
        setOpen(false);
      });
  }

  createEffect(() => {
    if (!open()) return;
    const onDown = (e: MouseEvent) => {
      if (!wrap.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    });
  });

  // Fixed hues per category, matte enough for both themes; free space stays
  // on the track colour.
  const rows = createMemo(() => {
    const d = detail();
    if (!d) return [];
    const all = [
      {
        label: `Messages · ${d.encodedMessages} of ${d.storedMessages}`,
        tokens: d.historyTokens,
        color: "#5f8bea",
      },
      { label: "System prompt", tokens: d.systemTokens, color: "#d76d9d" },
      { label: "Architecture map", tokens: d.architectureTokens, color: "#55a06e" },
      { label: "Tool schemas", tokens: d.toolsTokens, color: "#d9824a" },
    ];
    return all.filter((r) => r.tokens > 0).sort((a, b) => b.tokens - a.tokens);
  });
  const estTotal = () => rows().reduce((a, r) => a + r.tokens, 0);
  const freeTokens = () => Math.max(0, windowSize() - estTotal());
  const remaining = () => Math.max(0, windowSize() - used());
  const pctOfWindow = (t: number) => (windowSize() > 0 ? (t / windowSize()) * 100 : 0);

  return (
    <Show when={used() > 0 && windowSize() > 0}>
      <span class="ctx-wrap" ref={wrap}>
        <button
          type="button"
          class="ctx"
          data-tone={tone()}
          title={`Context: ${used().toLocaleString()} of ${windowSize().toLocaleString()} tokens (${pct().toFixed(1)}%), ${remaining().toLocaleString()} free — click for the breakdown`}
          onClick={toggle}
        >
          <span class="ctx-bar">
            <span class="ctx-fill" style={{ width: `${Math.max(2, pct())}%` }} />
          </span>
          <span class="tabular">
            {formatTokens(used())}/{formatTokens(windowSize())}
          </span>
        </button>

        <Show when={open()}>
          <div class="wsbar-menu ctx-pop">
            <div class="ctx-pop-head">
              <span>Context window</span>
              <span class="tabular">
                {formatTokens(used())} / {formatTokens(windowSize())} ({pct().toFixed(0)}%)
              </span>
            </div>
            <Show when={detail()} fallback={<div class="wsbar-menu-empty muted">measuring…</div>}>
              <div class="ctx-stack">
                <For each={rows()}>
                  {(r) => (
                    <span
                      class="ctx-seg"
                      style={{
                        width: `${Math.max(0.6, pctOfWindow(r.tokens))}%`,
                        background: r.color,
                      }}
                    />
                  )}
                </For>
              </div>
              <For each={rows()}>
                {(r) => (
                  <div class="ctx-row">
                    <span class="ctx-swatch" style={{ background: r.color }} />
                    <span class="ctx-row-label truncate">{r.label}</span>
                    <span class="ctx-row-tokens tabular">{formatTokens(r.tokens)}</span>
                    <span class="ctx-row-pct tabular">{pctOfWindow(r.tokens).toFixed(1)}%</span>
                  </div>
                )}
              </For>
              <div class="ctx-row" data-free>
                <span class="ctx-swatch" />
                <span class="ctx-row-label">Free space</span>
                <span class="ctx-row-tokens tabular">{formatTokens(freeTokens())}</span>
                <span class="ctx-row-pct tabular">{pctOfWindow(freeTokens()).toFixed(1)}%</span>
              </div>
              <div class="ctx-pop-note muted">
                Rows are estimates at 4 characters per token, measured from the real prompt, schemas and
                encoded history; the header is the last provider-billed size.
              </div>
            </Show>
          </div>
        </Show>
      </span>

      <Show when={props.session.messages.length >= 12 && props.session.live.status === "idle"}>
        <button
          type="button"
          class="wsbar-item wsbar-action"
          disabled={compacting()}
          title="Fold older history into a rolling structured summary; the recent tail stays verbatim"
          onClick={() => void compact()}
        >
          {compacting() ? "compacting…" : "compact"}
        </button>
      </Show>
    </Show>
  );
}
