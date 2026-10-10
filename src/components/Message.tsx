// A single transcript entry.
//
// Deliberately not a chat bubble. Entries are full-width with a narrow gutter
// carrying the role marker, so code and diffs get the whole column and long
// output reads like a document rather than a phone conversation.

import { createMemo, createSignal, For, Show } from "solid-js";
import { Markdown } from "./Markdown";
import { Brain, Check, Copy, Edit, Refresh, Undo, Warning } from "./Icons";
import { Logo } from "./Logo";
import { copyText, estimateTokens, formatCost, formatTokens } from "~/lib/actions";
import { store } from "~/lib/store";
import { ToolRow } from "./ToolRow";
import type { DesignMutator } from "~/lib/designDom";
import type { ToolActivity } from "~/lib/session";
import type { Message as Msg, Usage } from "~/lib/types";

interface Props {
  message: Msg;
  /** Tool calls this message requested, in the order they ran. */
  tools?: ToolActivity[];
  resultFor?: (callId: string) => string | null | undefined;
  onApprove?: (callId: string, approved: boolean) => void;
  onAlwaysAllow?: (risk: string) => void;
  onRetry?: () => void;
  onEdit?: (text: string) => void;
  onRevert?: (runId: string) => void;
  /** Commit a design-block edit (swatch recolour, area move…) to this message. */
  onDesignEdit?: (mutate: DesignMutator) => void;
  /** Open the sketch scratchpad seeded from a design block in this message. */
  onDesignSketch?: (designJson: string) => void;
  /** Render the strawberry mark in the gutter — only the newest assistant
   * entry carries it, so it reads as a status light: blinking while the run
   * is live, still once finished. */
  showMark?: boolean;
}

export function MessageView(props: Props) {
  const m = () => props.message;
  // Only a turn that actually wrote something offers an undo.
  const wroteFiles = () =>
    (props.tools ?? []).some((t) => (t.name === "write_file" || t.name === "edit_file") && t.status === "ok");
  const usage = createMemo<Usage | null>(() => {
    const raw = m().usage;
    if (!raw) return null;
    try {
      return JSON.parse(raw) as Usage;
    } catch {
      return null;
    }
  });

  return (
    <Show when={m().role !== "tool"} fallback={null}>
      <article class="entry" data-role={m().role}>
        <div class="entry-gutter">
          <Show when={m().role === "assistant"} fallback={<span class="entry-user-dot" />}>
            <Show when={props.showMark} fallback={<span class="entry-mark-dot" />}>
              <Logo size={16} />
            </Show>
          </Show>
        </div>

        <div class="entry-body">
          <Show when={m().role === "user"}>
            <UserBody message={m()} onEdit={props.onEdit} />
          </Show>

          <Show when={m().role === "assistant"}>
            <div class="entry-assistant">
              <Show when={m().reasoning}>{(r) => <Reasoning text={r()} />}</Show>
              <Show when={m().content}>
                <Markdown
                  text={m().content}
                  onDesignEdit={props.onDesignEdit}
                  onDesignSketch={props.onDesignSketch}
                />
              </Show>
              <For each={props.tools}>
                {(t) => (
                  <ToolRow
                    tool={t}
                    result={props.resultFor?.(t.id)}
                    onApprove={props.onApprove}
                    onAlwaysAllow={props.onAlwaysAllow}
                  />
                )}
              </For>
              <Show when={m().error}>
                {(err) => (
                  <div class="entry-error">
                    <Warning size={13} />
                    <span class="selectable">{err()}</span>
                  </div>
                )}
              </Show>
            </div>
          </Show>

          <div class="entry-actions">
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="Copy"
              onClick={() => void copyText(m().content)}
            >
              <Copy size={13} />
            </button>
            <Show when={m().role === "assistant" && props.onRetry}>
              <button
                type="button"
                class="btn"
                data-size="icon-sm"
                title="Retry from here"
                onClick={props.onRetry}
              >
                <Refresh size={13} />
              </button>
            </Show>
            <Show when={m().role === "assistant" && m().runId && wroteFiles() && props.onRevert}>
              <button
                type="button"
                class="btn"
                data-size="icon-sm"
                title="Revert the files this turn changed"
                onClick={() => props.onRevert?.(m().runId ?? "")}
              >
                <Undo size={13} />
              </button>
            </Show>
            <Show
              when={usage()}
              fallback={
                <span
                  class="entry-usage tabular muted"
                  title="Estimated — this message was not billed separately"
                >
                  ~{formatTokens(estimateTokens(m().content))} tok
                </span>
              }
            >
              {(u) => (
                <span class="entry-usage tabular muted" title="Billed by OpenRouter: input / output">
                  {formatTokens(u().prompt_tokens)}↑ {formatTokens(u().completion_tokens)}↓
                  <Show when={u().cost > 0}> · {formatCost(u().cost)}</Show>
                </span>
              )}
            </Show>
          </div>
        </div>
      </article>
    </Show>
  );
}

const LONG_MESSAGE_CHARS = 500;
const LONG_MESSAGE_LINES = 7;

function UserBody(props: { message: Msg; onEdit?: (text: string) => void }) {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal(props.message.content);
  const long = () =>
    props.message.content.length > LONG_MESSAGE_CHARS ||
    props.message.content.split("\n").length > LONG_MESSAGE_LINES;
  const [expanded, setExpanded] = createSignal(false);
  let field!: HTMLTextAreaElement;

  const commit = () => {
    const text = draft().trim();
    setEditing(false);
    // Unchanged text is the whole point of "resend" — never swallow it.
    if (text) props.onEdit?.(text);
  };

  return (
    <Show
      when={editing()}
      fallback={
        <div class="entry-user">
          <div class="entry-user-text selectable" data-collapsed={long() && !expanded()}>
            {props.message.content}
            <Show when={long()}>
              <button type="button" class="entry-user-more" onClick={() => setExpanded(!expanded())}>
                {expanded() ? "less" : "more"}
              </button>
            </Show>
          </div>
          <Show when={props.onEdit}>
            <button
              type="button"
              class="btn entry-edit"
              data-size="icon-sm"
              title="Resend this message (discards everything after it)"
              onClick={() => props.onEdit?.(props.message.content)}
            >
              <Refresh size={13} />
            </button>
            <button
              type="button"
              class="btn entry-edit"
              data-size="icon-sm"
              title="Edit and resend"
              onClick={() => {
                setDraft(props.message.content);
                setEditing(true);
                queueMicrotask(() => {
                  field.focus();
                  field.setSelectionRange(field.value.length, field.value.length);
                });
              }}
            >
              <Edit size={13} />
            </button>
          </Show>
        </div>
      }
    >
      <div class="entry-edit-box">
        <textarea
          ref={field}
          class="textarea"
          rows={Math.min(14, draft().split("\n").length + 1)}
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              setEditing(false);
            }
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              commit();
            }
          }}
        />
        <div class="entry-edit-actions">
          <button type="button" class="btn" data-variant="primary" data-size="sm" onClick={commit}>
            <Check size={12} /> Resend
          </button>
          <button type="button" class="btn" data-size="sm" onClick={() => setEditing(false)}>
            Cancel
          </button>
          <span class="muted" style={{ "font-size": "var(--fs-xs)" }}>
            Resending discards everything after this message
          </span>
        </div>
      </div>
    </Show>
  );
}

/** Collapsed by default: reasoning is context, not the answer. */
export function Reasoning(props: { text: string; streaming?: boolean }) {
  const [open, setOpen] = createSignal(false);
  const show = () => store.settings()?.showReasoning !== false;

  return (
    <Show when={show() && props.text.trim()}>
      <div class="reasoning" data-open={open()}>
        <button type="button" class="reasoning-head" onClick={() => setOpen(!open())}>
          <Brain size={12} />
          <span>Thinking</span>
          <Show when={props.streaming}>
            <span class="reasoning-pulse" />
          </Show>
          <span class="muted tabular" title="Estimated">
            ~{formatTokens(estimateTokens(props.text))} tok
          </span>
        </button>
        <Show when={open()}>
          <div class="reasoning-body selectable">{props.text}</div>
        </Show>
      </div>
    </Show>
  );
}
