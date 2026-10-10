// Per-chat runtime: the loaded transcript plus whatever is streaming into it.
//
// Sessions are cached by chat id and survive navigating away, so switching
// chats while a run is in flight is instant and loses nothing. The run itself
// lives in Rust; this is only the view of it.

import { createStore, produce, reconcile, type SetStoreFunction } from "solid-js/store";
import { createSignal, type Accessor, type Setter } from "solid-js";
import { api, Channel } from "./ipc";
import { store, toast } from "./store";
import { errorMessage } from "./types";
import type { AgentEvent, Message, ToolCallRecord } from "./types";
import { errText } from "~/lib/err";

export interface ToolActivity {
  id: string;
  /** The assistant message that requested this call. */
  messageId: string;
  name: string;
  args: string;
  status: "pending" | "awaiting" | "running" | "ok" | "error" | "declined";
  summary: string;
  display: Record<string, unknown> | null;
  risk?: string;
  /** Set when this call touches a protected credential path. */
  warning?: string;
  durationMs?: number;
  startedAt: number;
}

/** One stage of a graph run, as the transcript narrates it. */
export interface GraphStage {
  id: string;
  label: string;
  kind: string;
  status: string;
  detail: string | null;
  /** Rolling tail of the node's streamed output — the chat's live window
   * into what the stage is actually writing. */
  tail: string;
  cost: number;
  /** Verify/test outcome, rendered as a pass/fail card under the stage strip. */
  verdict: import("./meg").Verdict | null;
}

export interface GraphLive {
  name: string;
  stages: GraphStage[];
  /** "Verify failed — retry 2/3", while a loop is going around. */
  loopNote: string | null;
  finished: boolean;
}

export interface LiveState {
  runId: string | null;
  status: "idle" | "starting" | "streaming" | "tools" | "waiting" | "error";
  messageId: string | null;
  /** The assistant text currently arriving. Its own field so appends are cheap. */
  text: string;
  reasoning: string;
  step: number;
  /** Id of the assistant message currently being produced, or the last one. */
  currentMessageId: string | null;
  tokensIn: number;
  tokensOut: number;
  /** Prompt size of the most recent request — the live context size. */
  contextTokens: number;
  cost: number;
  startedAt: number;
  notice: string | null;
  error: string | null;
  retry: { attempt: number; delayMs: number } | null;
  /** Id of the tool call currently blocked on the user, if any. */
  approvalId: string | null;
  /** A question from the model, blocking the run until answered. */
  question: {
    id: string;
    question: string;
    options: string[];
    recommended: string | null;
    allowFreeText: boolean;
  } | null;
  /** A sketch node is parked on the user: the sketch pad resolves it. */
  sketch: { id: string; nodeId: string; prompt: string } | null;
  /** Live view of a graph run — the chat's backbone card while stages flow. */
  graph: GraphLive | null;
  /** Messages written while a run was busy, waiting to auto-send after it. */
  queued: string[];
}

const emptyLive = (): LiveState => ({
  runId: null,
  status: "idle",
  messageId: null,
  text: "",
  reasoning: "",
  step: 0,
  currentMessageId: null,
  tokensIn: 0,
  tokensOut: 0,
  contextTokens: 0,
  cost: 0,
  startedAt: 0,
  notice: null,
  error: null,
  retry: null,
  approvalId: null,
  question: null,
  sketch: null,
  graph: null,
  queued: [],
});

export interface Session {
  chatId: string;
  messages: Message[];
  setMessages: SetStoreFunction<Message[]>;
  toolCalls: Accessor<Map<string, ToolCallRecord>>;
  setToolCalls: Setter<Map<string, ToolCallRecord>>;
  live: LiveState;
  setLive: SetStoreFunction<LiveState>;
  /** Tool activity grouped by the assistant message that requested it. */
  toolsFor: (messageId: string) => ToolActivity[];
  orphanTools: () => ToolActivity[];
  plan: Accessor<import("./types").PlanDoc | null>;
  loaded: Accessor<boolean>;
  load: () => Promise<void>;
  send: (text: string, attachmentIds: string[]) => Promise<void>;
  retry: (seq: number, editedText: string | null) => Promise<void>;
  cancel: () => Promise<void>;
  /** Injects text into the running conversation (mid-run steering). */
  steer: (text: string) => Promise<void>;
  /** Holds text to auto-send when the current run finishes. */
  queueText: (text: string) => void;
  unqueue: (index: number) => void;
  approve: (callId: string, approved: boolean) => Promise<void>;
  answer: (questionId: string, text: string) => Promise<void>;
  dispose: () => void;
}

const sessions = new Map<string, Session>();

export function getSession(chatId: string): Session {
  let s = sessions.get(chatId);
  if (!s) {
    s = createSession(chatId);
    sessions.set(chatId, s);
  }
  return s;
}

export function dropSession(chatId: string) {
  sessions.get(chatId)?.dispose();
  sessions.delete(chatId);
}

function createSession(chatId: string): Session {
  const [messages, setMessages] = createStore<Message[]>([]);
  const [toolCalls, setToolCalls] = createSignal(new Map<string, ToolCallRecord>());
  const [live, setLive] = createStore<LiveState>(emptyLive());
  const [loaded, setLoaded] = createSignal(false);
  const [plan, setPlan] = createSignal<import("./types").PlanDoc | null>(null);

  // Tool rows live with the message that produced them, not with the run, so
  // they stay visible in the transcript after the run ends and after the next
  // message is sent.
  const [toolsByMessage, setToolsByMessage] = createStore<Record<string, ToolActivity[]>>({});
  const toolsFor = (messageId: string) => toolsByMessage[messageId] ?? [];

  /** Tool calls that never got attached to a committed message.
   *
   * This should stay empty, but an approval that renders nowhere is a silent
   * hang, so the live entry shows any orphans rather than losing them. */
  const orphanTools = () => toolsByMessage[""] ?? [];

  function upsertTool(id: string, update: (t: ToolActivity) => void) {
    setToolsByMessage(
      produce((byMessage) => {
        for (const list of Object.values(byMessage)) {
          const found = list.find((t) => t.id === id);
          if (found) {
            update(found);
            return;
          }
        }
      }),
    );
  }

  /** Maps a database tool status to its visual state. Anything that is not
   * definitively finished or in flight reads as an error — a spinner that can
   * never resolve is worse than a wrong-looking ✗. */
  function persistedStatus(status: string): ToolActivity["status"] {
    switch (status) {
      case "complete":
        return "ok";
      case "running":
      case "pending":
        return "running";
      case "declined":
        return "declined";
      default:
        return "error";
    }
  }

  /** Rebuilds `toolsByMessage` from persisted rows. */
  function adoptToolCalls(calls: ToolCallRecord[]) {
    setToolCalls(new Map(calls.map((c) => [c.id, c])));
    const grouped: Record<string, ToolActivity[]> = {};
    for (const c of calls) {
      const key = c.messageId ?? "";
      if (!key) continue;
      if (!grouped[key]) grouped[key] = [];
      grouped[key].push({
        id: c.id,
        messageId: key,
        name: c.name,
        args: c.args,
        status: persistedStatus(c.status),
        summary: (c.result ?? "").split("\n")[0] ?? "",
        display: null,
        durationMs: c.finishedAt ? c.finishedAt - c.startedAt : undefined,
        startedAt: c.startedAt,
      });
    }
    setToolsByMessage(reconcile(grouped));
  }

  async function load() {
    try {
      const [msgs, calls, storedPlan] = await Promise.all([
        api.messages(chatId),
        api.toolCalls(chatId),
        api.getPlan(chatId),
      ]);
      setMessages(msgs);
      adoptToolCalls(calls);
      setPlan(storedPlan);
      setLoaded(true);
    } catch (e) {
      // The chat is gone — deleted elsewhere, or left over from a previous
      // database. Retire the tab rather than leaving a broken view open.
      setLoaded(true);
      store.closeChatTab(chatId);
      dropSession(chatId);
      void store.refreshChats();
      toast("warn", `That chat no longer exists (${errorMessage(e)})`);
    }
  }

  /** Pulls fresh state from the database after a gap in the event stream. */
  async function resync() {
    try {
      const [msgs, calls] = await Promise.all([api.messages(chatId), api.toolCalls(chatId)]);
      setMessages(msgs);
      setToolCalls(new Map(calls.map((c) => [c.id, c])));
    } catch {
      /* the next event will try again */
    }
  }

  function apply(ev: AgentEvent) {
    switch (ev.type) {
      case "runStarted":
        setLive(
          produce((l) => {
            Object.assign(l, emptyLive());
            l.runId = ev.runId;
            l.status = "starting";
            l.startedAt = Date.now();
          }),
        );
        store.markRunning(chatId, true);
        break;

      case "stepStarted":
        setLive("step", ev.step + 1);
        break;

      case "messageStarted":
        setLive(
          produce((l) => {
            l.messageId = ev.messageId;
            // Tool calls arrive after this message is committed, and attach to
            // it by id — so this has to be recorded, not just the live pointer.
            l.currentMessageId = ev.messageId;
            l.text = "";
            l.reasoning = "";
            l.status = "streaming";
          }),
        );
        break;

      // The hot path. One string append per coalesced batch, nothing else.
      case "delta":
        setLive("text", (t) => t + ev.text);
        break;

      case "reasoning":
        setLive("reasoning", (t) => t + ev.text);
        break;

      case "toolCallStarted": {
        // Tool calls arrive after their assistant message has been committed,
        // so they attach to the most recent one.
        const owner = live.currentMessageId ?? "";
        setToolsByMessage(
          produce((byMessage) => {
            if (!byMessage[owner]) byMessage[owner] = [];
            byMessage[owner].push({
              id: ev.id,
              messageId: owner,
              name: ev.name,
              args: "",
              status: "pending",
              summary: "",
              display: null,
              startedAt: Date.now(),
            });
          }),
        );
        break;
      }

      case "toolCallArgs":
        upsertTool(ev.id, (t) => {
          t.args += ev.delta;
        });
        break;

      case "planUpdated":
        setPlan({ title: ev.title, markdown: ev.markdown, updatedAt: Date.now() });
        // Surface the pane the first time a plan appears for the chat being
        // looked at; a background chat's plan stays put until visited.
        if (store.activeChatId() === chatId) store.openPane({ kind: "plan" });
        break;

      case "userQuestion":
        setLive(
          produce((l) => {
            l.question = {
              id: ev.id,
              question: ev.question,
              options: ev.options,
              recommended: ev.recommended,
              allowFreeText: ev.allowFreeText,
            };
            l.status = "waiting";
          }),
        );
        break;

      case "sketchRequest":
        setLive(
          produce((l) => {
            l.sketch = { id: ev.id, nodeId: ev.nodeId, prompt: ev.prompt };
            l.status = "waiting";
          }),
        );
        // Bring the pad to the person whose drawing the run is waiting on.
        if (store.activeChatId() === chatId) {
          store.openPane({ kind: "sketch" });
          toast("info", "The graph is waiting for your sketch — draw it and press Send.");
        }
        break;

      case "toolApproval":
        upsertTool(ev.id, (t) => {
          t.status = "awaiting";
          t.risk = ev.risk;
          t.warning = ev.warning ?? undefined;
          t.args = JSON.stringify(ev.args ?? {});
        });
        setLive(
          produce((l) => {
            l.status = "waiting";
            l.approvalId = ev.id;
          }),
        );
        break;

      case "toolStarted":
        upsertTool(ev.id, (t) => {
          t.status = "running";
          t.args = JSON.stringify(ev.args ?? {});
        });
        setLive(
          produce((l) => {
            if (l.approvalId === ev.id) l.approvalId = null;
            l.status = "tools";
          }),
        );
        break;

      case "toolFinished":
        upsertTool(ev.id, (t) => {
          t.status = ev.ok ? "ok" : "error";
          t.summary = ev.summary;
          t.display = ev.display;
          t.durationMs = ev.durationMs;
        });
        setLive("approvalId", (cur) => (cur === ev.id ? null : cur));
        // The tool's result message is now in the database.
        void refreshToolCall(ev.id);
        break;

      case "messageFinished":
        // Commit the streamed text as a real message and clear the live buffer,
        // so exactly one copy is ever on screen.
        setMessages(
          produce((list) => {
            const seq = list.length ? list[list.length - 1]!.seq + 1 : 1;
            list.push({
              id: ev.messageId,
              chatId,
              seq,
              role: "assistant",
              content: ev.content,
              reasoning: ev.reasoning,
              toolCalls: null,
              toolCallId: null,
              model: null,
              status: "complete",
              error: null,
              usage: ev.usage ? JSON.stringify(ev.usage) : null,
              runId: live.runId,
              createdAt: Date.now(),
            });
          }),
        );
        setLive(
          produce((l) => {
            l.text = "";
            l.reasoning = "";
            l.messageId = null;
          }),
        );
        break;

      case "usage":
        setLive(
          produce((l) => {
            l.tokensIn += ev.promptTokens;
            l.tokensOut += ev.completionTokens;
            l.cost += ev.cost;
            // Each step's prompt is the whole conversation as sent, so the
            // latest one is the current context size.
            l.contextTokens = ev.promptTokens;
          }),
        );
        break;

      case "retry":
        setLive("retry", { attempt: ev.attempt, delayMs: ev.delayMs });
        break;

      case "notice":
        if (ev.level === "resync") void resync();
        else setLive("notice", ev.text);
        if (ev.level === "error") toast("error", ev.text);
        break;

      case "titleChanged":
        store.setChats((cs) => cs.map((c) => (c.id === ev.chatId ? { ...c, title: ev.title } : c)));
        break;

      // ---- graph runs: the pipeline card the transcript narrates.
      case "graphStarted":
        setLive("graph", {
          name: ev.name,
          stages: ev.nodes.map((n) => ({
            ...n,
            status: "pending",
            detail: null,
            tail: "",
            cost: 0,
            verdict: null,
          })),
          loopNote: null,
          finished: false,
        });
        break;

      case "graphNode":
        setLive(
          produce((l) => {
            const s = l.graph?.stages.find((x) => x.id === ev.nodeId);
            if (s) {
              s.status = ev.status;
              s.detail = ev.detail;
            }
          }),
        );
        break;

      // The streamed text of whichever node is working — without this the
      // chat shows a static card for the whole run and every word happens
      // off-screen in the graph pane.
      case "graphNodeDelta":
        setLive(
          produce((l) => {
            const s = l.graph?.stages.find((x) => x.id === ev.nodeId);
            if (!s) return;
            s.tail = (s.tail + ev.text).slice(-900);
          }),
        );
        break;

      case "graphNodeDone":
        setLive(
          produce((l) => {
            const s = l.graph?.stages.find((x) => x.id === ev.nodeId);
            if (!s) return;
            s.status = ev.status;
            s.detail = null;
            s.tail = "";
            const art = ev.artifact as { cost?: number } | null;
            if (art?.cost) s.cost = art.cost;
            const v = ev.verdict as import("./meg").Verdict | null;
            s.verdict = v && typeof v.pass === "boolean" ? v : null;
            if (l.graph) l.graph.loopNote = null;
          }),
        );
        break;

      case "graphLoop":
        setLive(
          produce((l) => {
            if (l.graph) l.graph.loopNote = `didn't pass — retry ${ev.iteration}/${ev.maxIters}`;
          }),
        );
        break;

      case "runFinished":
        setLive(
          produce((l) => {
            l.status = ev.error ? "error" : "idle";
            l.error = ev.error;
            l.runId = null;
            l.retry = null;
            l.approvalId = null;
            l.question = null;
            l.sketch = null;
            // The graph card stays on screen as the run's record; the next
            // run replaces it.
            if (l.graph) {
              l.graph.finished = true;
              l.graph.loopNote = null;
            }
          }),
        );
        store.markRunning(chatId, false);
        flushQueue();
        // Reconcile against the database: user messages, tool messages and any
        // final edits the stream did not carry.
        void resync();
        void store.refreshChats();
        if (ev.error) toast("error", ev.error);
        break;
    }
  }

  async function refreshToolCall(id: string) {
    try {
      const calls = await api.toolCalls(chatId);
      const found = calls.find((c) => c.id === id);
      if (found) setToolCalls((m) => new Map(m).set(id, found));
    } catch {
      /* non-critical */
    }
  }

  function channel() {
    const ch = new Channel<AgentEvent>();
    ch.onmessage = apply;
    return ch;
  }

  async function send(text: string, attachmentIds: string[]) {
    // Show the user's message immediately; the backend confirms it on resync.
    setMessages(
      produce((list) => {
        list.push({
          id: `pending-${Date.now()}`,
          chatId,
          seq: list.length ? list[list.length - 1]!.seq + 1 : 1,
          role: "user",
          content: text,
          reasoning: null,
          toolCalls: null,
          toolCallId: null,
          model: null,
          status: "complete",
          error: null,
          usage: null,
          runId: null,
          createdAt: Date.now(),
        });
      }),
    );
    setLive(
      produce((l) => {
        const queued = l.queued;
        Object.assign(l, emptyLive());
        l.queued = queued;
        l.status = "starting";
        l.startedAt = Date.now();
      }),
    );

    try {
      await api.sendMessage(chatId, text, attachmentIds, channel());
    } catch (e) {
      setLive(
        produce((l) => {
          l.status = "error";
          l.error = errText(e);
        }),
      );
      store.markRunning(chatId, false);
      void resync();
      throw e;
    }
  }

  async function retry(seq: number, editedText: string | null) {
    // Rewind locally — and when this is a resend of a user message, put it
    // straight back optimistically. The backend re-inserts it too, but the
    // database reconcile only lands at run end; without this, resending the
    // first message left the transcript headless for the whole run.
    setMessages((list) => {
      const kept = list.filter((m) => m.seq < seq);
      if (editedText !== null) {
        kept.push({
          id: `pending-${Date.now()}`,
          chatId,
          seq,
          role: "user",
          content: editedText,
          reasoning: null,
          toolCalls: null,
          toolCallId: null,
          model: null,
          status: "complete",
          error: null,
          usage: null,
          runId: null,
          createdAt: Date.now(),
        });
      }
      return kept;
    });
    setLive(
      produce((l) => {
        Object.assign(l, emptyLive());
        l.status = "starting";
        l.startedAt = Date.now();
      }),
    );
    try {
      await api.retryFrom(chatId, seq, editedText, channel());
    } catch (e) {
      setLive(
        produce((l) => {
          l.status = "error";
          l.error = errText(e);
        }),
      );
      void resync();
      throw e;
    }
  }

  async function cancel() {
    const id = live.runId;
    if (id) {
      try {
        await api.cancelRun(id);
        return;
      } catch {
        // The backend no longer knows this run — fall through to the reset.
      }
    }
    // Nothing cancellable, yet the UI thinks it is busy (a run that died
    // before announcing itself, or stale state): unwedge locally so the
    // composer and Stop are never dead controls.
    setLive(
      produce((l) => {
        l.status = "idle";
        l.runId = null;
      }),
    );
    store.markRunning(chatId, false);
    void resync();
  }

  /** Appends `text` to the running conversation. The runner folds it into its
   * next step (and takes an extra step if it was about to stop), so this is
   * live steering. Falls back to the queue when the run just ended. */
  async function steer(text: string) {
    setMessages(
      produce((list) => {
        list.push({
          id: `pending-${Date.now()}`,
          chatId,
          seq: list.length ? list[list.length - 1]!.seq + 1 : 1,
          role: "user",
          content: text,
          reasoning: null,
          toolCalls: null,
          toolCallId: null,
          model: null,
          status: "complete",
          error: null,
          usage: null,
          runId: null,
          createdAt: Date.now(),
        });
      }),
    );
    try {
      await api.runSteer(chatId, text);
    } catch {
      // Raced the run's end: undo the optimistic row and queue it instead.
      setMessages((list) => list.filter((m) => !m.id.startsWith("pending-")));
      queueText(text);
    }
  }

  /** Holds a message to auto-send the moment the current run finishes. */
  function queueText(text: string) {
    setLive("queued", (q) => [...q, text]);
    if (live.status === "idle" || live.status === "error") flushQueue();
  }

  function unqueue(index: number) {
    setLive("queued", (q) => q.filter((_, i) => i !== index));
  }

  function flushQueue() {
    const q = live.queued;
    if (q.length === 0) return;
    const text = q.join("\n\n");
    setLive("queued", []);
    // A beat after runFinished, so the finish reconcile lands first.
    setTimeout(() => {
      void send(text, []).catch(() => {
        // send() already surfaced the error state; put the text back.
        setLive("queued", (cur) => [text, ...cur]);
      });
    }, 300);
  }

  async function answer(questionId: string, text: string) {
    const id = live.runId;
    if (!id) return;
    setLive(
      produce((l) => {
        l.question = null;
        l.status = "tools";
      }),
    );
    await api.answerQuestion(id, questionId, text);
  }

  async function approve(callId: string, approved: boolean) {
    const id = live.runId;
    if (!id) return;
    if (!approved) {
      upsertTool(callId, (t) => {
        t.status = "declined";
      });
    }
    setLive("approvalId", null);
    await api.approveTool(id, callId, approved);
  }

  /** Reattaches to a run that is still going after a reload. */
  async function reattach(runId: string) {
    try {
      await api.subscribeRun(runId, channel());
    } catch {
      /* run already gone */
    }
  }

  void api
    .activeRuns()
    .then((runs) => {
      const mine = runs.find((r) => r.chatId === chatId);
      if (mine) {
        setLive(
          produce((l) => {
            l.runId = mine.id;
            l.status = "streaming";
            l.startedAt = mine.startedAt;
          }),
        );
        void reattach(mine.id);
      }
    })
    .catch(() => {});

  return {
    chatId,
    messages,
    setMessages,
    toolCalls,
    setToolCalls,
    toolsFor,
    orphanTools,
    live,
    setLive,
    loaded,
    load,
    send,
    retry,
    cancel,
    steer,
    queueText,
    unqueue,
    approve,
    answer,
    plan,
    dispose: () => {
      /* channels close with the run */
    },
  };
}
