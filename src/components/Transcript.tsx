// The scrolling transcript.
//
// Scrolling is the thing people feel most, so it is handled explicitly rather
// than left to the browser:
//
//  * Native scroll anchoring is switched off and replaced with a "stuck to
//    bottom" rule, because anchoring fights a list whose last item grows.
//  * The pin is applied in a single rAF per frame, so sixty token batches a
//    second still cost one scroll write.
//  * Entries carry `content-visibility: auto`, so a thousand-message chat only
//    lays out what is on screen while staying fully selectable and searchable.

import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { MessageView, Reasoning } from "./Message";
import { QuestionCard } from "./QuestionCard";
import { ToolRow } from "./ToolRow";
import { Markdown } from "./Markdown";
import { Logo } from "./Logo";
import { openSketchFromDesign } from "./SketchPad";
import { ChevronDown, NodeGraph } from "./Icons";
import { api } from "~/lib/ipc";
import { designBlockOf, NODE_TYPE, updateDesignJson } from "~/lib/meg";
import type { NodeKind } from "~/lib/meg";
import { megState } from "~/lib/megState";
import type { DesignMutator } from "~/lib/designDom";
import type { Session } from "~/lib/session";
import type { Message as Msg } from "~/lib/types";
import { formatCost } from "~/lib/actions";
import { store, toast } from "~/lib/store";
import { errText } from "~/lib/err";

const STICK_THRESHOLD = 72;

export function Transcript(props: { session: Session }) {
  let scroller!: HTMLDivElement;
  const [stuck, setStuck] = createSignal(true);
  const [hasOverflow, setHasOverflow] = createSignal(false);
  let pinScheduled = false;

  const atBottom = () =>
    scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= STICK_THRESHOLD;

  const pin = () => {
    if (pinScheduled) return;
    pinScheduled = true;
    requestAnimationFrame(() => {
      pinScheduled = false;
      if (stuck()) scroller.scrollTop = scroller.scrollHeight;
    });
  };

  const onScroll = () => {
    const bottom = atBottom();
    if (bottom !== stuck()) setStuck(bottom);
    setHasOverflow(scroller.scrollHeight > scroller.clientHeight + 8);
  };

  onMount(() => {
    scroller.scrollTop = scroller.scrollHeight;
    const ro = new ResizeObserver(() => {
      onScroll();
      pin();
    });
    ro.observe(scroller);
    onCleanup(() => ro.disconnect());
  });

  // Every source of growth funnels into the same pin.
  createEffect(on(() => props.session.live.text, pin));
  createEffect(on(() => props.session.live.reasoning, pin));
  createEffect(on(() => props.session.live.status, pin));
  createEffect(on(() => props.session.live.graph?.stages.map((s) => s.status).join(","), pin));
  createEffect(
    on(
      () => props.session.messages.length,
      () => {
        setStuck(true);
        pin();
      },
    ),
  );

  const jump = () => {
    setStuck(true);
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
  };

  const live = () => props.session.live;
  const streaming = () => live().status !== "idle" && live().status !== "error";
  const results = () => props.session.toolCalls();

  // Raising the permission mode by exactly one step: approving one edit should
  // not silently grant the agent the shell.
  const alwaysAllow = (risk: string) => {
    const s = store.settings();
    if (!s) return;
    void store.setSettings({ ...s, permissionMode: risk === "write" ? "acceptEdits" : "auto" });
  };

  const approve = (callId: string, ok: boolean) => void props.session.approve(callId, ok);

  // The mark sits on the newest assistant message — and only while nothing is
  // streaming, because during a run the live entry's blinking mark is the
  // status light.
  const lastAssistantId = () => {
    if (streaming()) return null;
    for (let i = props.session.messages.length - 1; i >= 0; i--) {
      const m = props.session.messages[i];
      if (m?.role === "assistant") return m.id;
    }
    return null;
  };

  /** Persists a design edit made directly on a chat message — a swatch
   * recoloured, an area dragged — and mirrors it into the bound graph's
   * design artifact when that artifact holds the same design. */
  const designEditFor = (m: Msg) => (mutate: DesignMutator) => {
    if (props.session.live.runId) {
      toast("warn", "Wait for the current run to finish before editing the design.");
      return;
    }
    const next = updateDesignJson(m.content, mutate);
    if (!next || next === m.content) return;
    const origBlock = designBlockOf(m.content);
    void api
      .updateMessage(m.id, next)
      .then(() => {
        props.session.setMessages((list) => list.map((x) => (x.id === m.id ? { ...x, content: next } : x)));
        void mirrorDesignToGraph(props.session.chatId, origBlock, mutate);
      })
      .catch((e) => toast("error", errText(e)));
  };

  async function revertRun(runId: string) {
    try {
      const results = await api.revertRun(runId, false);
      toast("ok", `Reverted ${results.length} file${results.length === 1 ? "" : "s"}`);
    } catch (e) {
      const text = errText(e);
      if (text.includes("changed since")) {
        toast("warn", "Files changed since that turn", {
          label: "Revert anyway",
          run: () => void api.revertRun(runId, true).then(() => toast("ok", "Reverted")),
        });
      } else {
        toast("error", text);
      }
    }
  }

  return (
    <div class="transcript-wrap">
      <div class="transcript" ref={scroller} onScroll={onScroll}>
        <div class="transcript-inner">
          <For each={props.session.messages} fallback={<EmptyChat />}>
            {(m) => (
              <MessageView
                message={m}
                showMark={m.id === lastAssistantId()}
                tools={props.session.toolsFor(m.id)}
                resultFor={(id) => results().get(id)?.result}
                onApprove={approve}
                onAlwaysAllow={alwaysAllow}
                onRetry={m.role === "assistant" ? () => void props.session.retry(m.seq, null) : undefined}
                onRevert={(runId) => void revertRun(runId)}
                onEdit={m.role === "user" ? (text) => void props.session.retry(m.seq, text) : undefined}
                onDesignEdit={designEditFor(m)}
                onDesignSketch={openSketchFromDesign}
              />
            )}
          </For>

          <Show when={live().graph}>
            <GraphRunCard session={props.session} />
          </Show>

          <Show when={streaming() || live().text || live().reasoning}>
            <LiveEntry session={props.session} />
          </Show>

          <div class="transcript-tail" />
        </div>
      </div>

      <Show when={!stuck() && hasOverflow()}>
        <button type="button" class="jump-latest" onClick={jump}>
          <ChevronDown size={13} /> Latest
        </button>
      </Show>
    </div>
  );
}

/** Best-effort: when the chat is bound to a graph whose design node produced
 * this exact design, the hand edit flows into that artifact too (pinning the
 * node), so the next run serves the user's version instead of overwriting it. */
async function mirrorDesignToGraph(chatId: string, origBlock: string | null, mutate: DesignMutator) {
  if (!origBlock) return;
  try {
    const gid = await api.graphBinding(chatId);
    if (!gid) return;
    if (!megState.loaded()) await megState.load();
    const g = megState.graphs().find((x) => x.id === gid);
    if (!g) return;
    for (const n of g.spec.nodes) {
      if (n.kind !== "design") continue;
      const art = g.artifacts[n.id];
      if (!art?.content.includes(origBlock)) continue;
      const next = updateDesignJson(art.content, mutate);
      if (next && next !== art.content) {
        megState.upsert(await api.graphSetArtifact(gid, n.id, next));
        toast("ok", "Design updated — the graph's Design node is pinned with your edit.");
      }
    }
  } catch {
    /* mirroring is a convenience; the message edit already landed */
  }
}

/**
 * The graph run's backbone in the chat: every stage as a chip, lighting up as
 * the flow reaches it, with loop retries narrated and the total cost live.
 * It stays after the run finishes as the record of how the answer was made.
 */
function GraphRunCard(props: { session: Session }) {
  const g = () => props.session.live.graph;
  const glyph = (kind: string) => NODE_TYPE[kind as NodeKind]?.glyph ?? "•";
  const statusMark = (s: string) =>
    s === "done" ? "✓" : s === "cached" ? "⟲" : s === "failed" ? "✗" : s === "skipped" ? "–" : "";

  return (
    <Show when={g()}>
      {(graph) => (
        <div class="graph-card" data-finished={graph().finished}>
          <div class="graph-card-head">
            <button
              type="button"
              class="graph-card-name"
              title="Open the execution graph  ⌘⇧E"
              onClick={() => store.openPane({ kind: "graph" })}
            >
              <NodeGraph size={12} /> {graph().name}
            </button>
            <Show when={graph().loopNote}>
              {(n) => (
                <span class="chip" data-tone="accent">
                  ↺ {n()}
                </span>
              )}
            </Show>
            <div class="spacer" />
            <Show when={props.session.live.cost > 0}>
              <span class="chip tabular">{formatCost(props.session.live.cost)}</span>
            </Show>
          </div>
          <div class="graph-card-stages">
            <For each={graph().stages}>
              {(s, i) => (
                <>
                  <Show when={i() > 0}>
                    <span class="graph-card-sep">→</span>
                  </Show>
                  <span class="graph-card-stage" data-status={s.status} title={s.detail ?? s.status}>
                    <i>{glyph(s.kind)}</i> {s.label}
                    <Show when={statusMark(s.status)}>{(m) => <em data-status={s.status}>{m()}</em>}</Show>
                  </span>
                </>
              )}
            </For>
          </div>
          <Show when={g()?.stages.find((s) => s.status === "running")?.detail}>
            {(d) => <div class="graph-card-detail truncate">{d()}</div>}
          </Show>
          {/* Verification outcomes as cards: what passed, what failed, and the
              concrete issues driving the retry loop — live, in the chat. */}
          <For each={graph().stages.filter((s) => s.verdict)}>
            {(s) => (
              <div class="graph-card-verdict" data-pass={s.verdict!.pass}>
                <div class="graph-card-verdict-head">
                  <strong>
                    {s.verdict!.pass ? "✓" : "✗"} {s.label}
                  </strong>
                  <Show when={s.verdict!.score !== null}>
                    <span class="tabular">{Math.round((s.verdict!.score ?? 0) * 100)}%</span>
                  </Show>
                  <Show when={s.verdict!.summary}>
                    <span class="graph-card-verdict-summary truncate">{s.verdict!.summary}</span>
                  </Show>
                </div>
                <Show when={!s.verdict!.pass}>
                  <For each={s.verdict!.issues.slice(0, 4)}>
                    {(i) => <div class="graph-card-issue">• {i}</div>}
                  </For>
                  <Show when={s.verdict!.issues.length > 4}>
                    <div class="graph-card-issue muted">
                      … {s.verdict!.issues.length - 4} more in the graph pane
                    </div>
                  </Show>
                </Show>
              </div>
            )}
          </For>
        </div>
      )}
    </Show>
  );
}

/**
 * The in-flight assistant turn.
 *
 * Only prose and reasoning live here — tool calls attach to the message that
 * requested them, which is what keeps them in the transcript after the run
 * finishes.
 */
function LiveEntry(props: { session: Session }) {
  const live = () => props.session.live;

  return (
    <article class="entry" data-role="assistant" data-live="true">
      <div class="entry-gutter">
        <span class="entry-live-mark">
          <Logo size={16} />
        </span>
      </div>
      <div class="entry-body">
        <Show when={live().reasoning}>
          <Reasoning text={live().reasoning} streaming />
        </Show>

        <Show when={live().text}>
          <Markdown text={live().text} streaming />
        </Show>

        {/* Safety net: a tool call with no owning message would otherwise be
            invisible, and an unreachable approval blocks the run forever. */}
        <For each={props.session.orphanTools()}>
          {(t) => <ToolRow tool={t} onApprove={(id, ok) => void props.session.approve(id, ok)} />}
        </For>

        <Show when={live().question}>
          {(q) => (
            <QuestionCard
              question={q().question}
              options={q().options}
              recommended={q().recommended}
              allowFreeText={q().allowFreeText}
              onAnswer={(text) => void props.session.answer(q().id, text)}
            />
          )}
        </Show>

        <Show when={live().retry}>
          {(r) => (
            <div class="entry-notice">
              Connection dropped — retrying (attempt {r().attempt}) in {Math.round(r().delayMs / 100) / 10}s
            </div>
          )}
        </Show>

        <Show when={live().notice}>{(n) => <div class="entry-notice">{n()}</div>}</Show>

        <Show when={live().status === "starting" && !live().text}>
          <div class="thinking-dots" role="status" aria-label="Working">
            <span />
            <span />
            <span />
          </div>
        </Show>

        <Show when={live().error}>{(e) => <div class="entry-error selectable">{e()}</div>}</Show>
      </div>
    </article>
  );
}

function EmptyChat() {
  return (
    <div class="chat-empty">
      <Logo size={40} />
      <h2>What are we building?</h2>
      <p>
        Ask a question, or point at a folder and let the agent read, search, edit and run things for you.
        Everything it changes is tracked and can be reverted.
      </p>
      <div class="chat-empty-hints">
        <span>
          <kbd>⌘</kbd>
          <kbd>K</kbd> commands
        </span>
        <span>
          <kbd>⌘</kbd>
          <kbd>J</kbd> side pane
        </span>
        <span>
          <kbd>⌘</kbd>
          <kbd>↵</kbd> send
        </span>
      </div>
    </div>
  );
}
