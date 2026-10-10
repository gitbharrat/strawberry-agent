// The run bar.
//
// While an agent is working, the questions are always the same: what is it
// doing, how long has it been going, what has it cost, and how do I stop it.
// This answers all four in one strip that is always visible — the thing most
// harnesses make you hunt for.

import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Stop } from "./Icons";
import { describeTool } from "./ToolRow";
import { formatCost, formatTokens } from "~/lib/actions";
import type { Session, ToolActivity } from "~/lib/session";

export function RunBar(props: { session: Session }) {
  const [now, setNow] = createSignal(Date.now());
  const live = () => props.session.live;
  const active = () => live().status !== "idle" && live().status !== "error";

  // One timer, only while a run is live.
  createEffect(() => {
    if (!active()) return;
    const id = setInterval(() => setNow(Date.now()), 250);
    onCleanup(() => clearInterval(id));
  });

  const tokRate = () => {
    const l = live();
    const secs = (now() - l.startedAt) / 1000;
    if (secs < 2 || l.tokensOut === 0) return 0;
    return Math.round(l.tokensOut / secs);
  };

  const elapsed = () => {
    const start = live().startedAt;
    if (!start) return "0.0s";
    const ms = now() - start;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
    return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
  };

  const currentTools = (): ToolActivity[] =>
    props.session.toolsFor(props.session.live.currentMessageId ?? "");

  const activity = () => {
    const l = live();
    const tools = currentTools();
    if (l.question) return "waiting for your answer";
    if (l.approvalId) {
      const pending = tools.find((t) => t.id === l.approvalId);
      return `waiting for you — ${pending?.name ?? "approval"}`;
    }
    const running = [...tools].reverse().find((t) => t.status === "running" || t.status === "pending");
    if (running) return `${running.name}  ${describeTool(running.name, running.args)}`;
    if (l.status === "streaming") return "writing";
    if (l.status === "starting") return "thinking";
    return "working";
  };

  return (
    <Show when={active()}>
      <div class="runbar" data-waiting={!!live().approvalId || !!live().question}>
        <span class="runbar-pulse" />
        <span class="runbar-activity truncate">{activity()}</span>
        <span class="vdivider" />
        <span class="runbar-stat tabular" title="Elapsed">
          {elapsed()}
        </span>
        <Show when={live().step > 0}>
          <span class="runbar-stat tabular" title="Agent steps">
            step {live().step}
          </span>
        </Show>
        <Show when={live().status === "streaming" && tokRate() > 0}>
          <span class="runbar-stat tabular" title="Output tokens per second">
            {tokRate()} tok/s
          </span>
        </Show>
        <Show when={live().tokensIn + live().tokensOut > 0}>
          <span class="runbar-stat tabular" title="Tokens in / out">
            {formatTokens(live().tokensIn)}↑ {formatTokens(live().tokensOut)}↓
          </span>
        </Show>
        <Show when={live().cost > 0}>
          <span class="runbar-stat tabular" title="Cost so far">
            {formatCost(live().cost)}
          </span>
        </Show>
        <div class="spacer" />
        <button
          type="button"
          class="btn"
          data-size="sm"
          data-variant="danger"
          onClick={() => void props.session.cancel()}
        >
          <Stop size={11} /> Stop
        </button>
      </div>
    </Show>
  );
}
