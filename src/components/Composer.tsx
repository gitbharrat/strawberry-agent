// The input.
//
// A textarea that grows to fit, plus attachments and the model picker inline —
// no modal, no separate settings trip. Enter sends; Shift+Enter is a newline,
// which is the convention every developer already has in their fingers.

import { createSignal, For, Show, onMount } from "solid-js";
import { Attach, Close, Edit, File as FileIcon, Image as ImageIcon, NodeGraph, Send, Stop } from "./Icons";
import { ModelPicker } from "./ModelPicker";
import { WorkspaceBar } from "./WorkspaceBar";
import { api } from "~/lib/ipc";
import { formatBytes, pickFiles } from "~/lib/actions";
import { megState } from "~/lib/megState";
import { store, toast } from "~/lib/store";
import type { Session } from "~/lib/session";
import type { Attachment } from "~/lib/types";
import { errText } from "~/lib/err";

const MAX_ROWS = 16;

/** Reads a clipboard file as base64, without the data-URL prefix. */
function toBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("could not read the file"));
    reader.onload = () => {
      const result = String(reader.result);
      const comma = result.indexOf(",");
      resolve(comma === -1 ? result : result.slice(comma + 1));
    };
    reader.readAsDataURL(file);
  });
}

function extensionFor(mime: string): string {
  const map: Record<string, string> = {
    "image/png": "png",
    "image/jpeg": "jpg",
    "image/gif": "gif",
    "image/webp": "webp",
    "image/avif": "avif",
    "application/pdf": "pdf",
  };
  return map[mime] ?? "bin";
}

function stamp(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
}

export function Composer(props: { session: Session; chatId: string }) {
  const [text, setText] = createSignal("");
  const [pending, setPending] = createSignal<Attachment[]>([]);
  const [dragging, setDragging] = createSignal(false);
  let field!: HTMLTextAreaElement;

  const busy = () => props.session.live.status !== "idle" && props.session.live.status !== "error";
  const canSend = () => text().trim().length > 0 || pending().length > 0;
  // Steering rides the live run's own conversation; graph runs consume one
  // goal per invocation, and attachments bind at send time — both go to the
  // queue and fire the moment the run ends.
  const steerable = () => busy() && !megState.binding(props.chatId) && pending().length === 0;

  const autosize = () => {
    field.style.height = "auto";
    const lineHeight = 21;
    const max = MAX_ROWS * lineHeight + 16;
    field.style.height = `${Math.min(field.scrollHeight, max)}px`;
    field.style.overflowY = field.scrollHeight > max ? "auto" : "hidden";
  };

  onMount(() => {
    field.focus();
    autosize();
  });

  /**
   * Clipboard paste.
   *
   * A screenshot on the clipboard has no path on disk, so the bytes go straight
   * to the backend. Plain text is left to the browser's own handling.
   */
  async function onPaste(e: ClipboardEvent) {
    const files = Array.from(e.clipboardData?.files ?? []);
    if (files.length === 0) return;
    e.preventDefault();

    for (const file of files) {
      try {
        const base64 = await toBase64(file);
        const name = file.name || `pasted-${stamp()}.${extensionFor(file.type)}`;
        const added = await api.attachBytes(
          props.chatId,
          name,
          file.type || "application/octet-stream",
          base64,
        );
        setPending((p) => [...p, added]);
      } catch (err) {
        toast("error", `Could not attach that paste: ${String(err)}`);
      }
    }
  }

  async function attach(paths: string[]) {
    if (!paths.length) return;
    try {
      const added = await api.attachFiles(props.chatId, paths);
      setPending((p) => [...p, ...added]);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function send() {
    if (!canSend()) return;
    const body = text().trim();
    const ids = pending().map((a) => a.id);

    if (busy()) {
      // Mid-run: steer the conversation live, or queue for right after.
      if (!body && ids.length === 0) return;
      setText("");
      queueMicrotask(autosize);
      if (steerable()) {
        await props.session.steer(body);
      } else {
        props.session.queueText(body);
      }
      return;
    }

    setText("");
    setPending([]);
    queueMicrotask(autosize);
    // A bound chat runs through its execution graph — bring the graph into
    // view so the run is watchable as it flows.
    if (megState.binding(props.chatId)) store.openPane({ kind: "graph" });
    try {
      await props.session.send(body, ids);
    } catch (e) {
      // Put the text back so nothing is lost on a rejected send.
      setText(body);
      queueMicrotask(autosize);
      toast("error", errText(e));
    }
  }

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
      return;
    }
    // Escape stops a run without leaving the field.
    if (e.key === "Escape" && busy()) {
      e.preventDefault();
      void props.session.cancel();
    }
  };

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: drop target for file attachments; the picker button is the keyboard path
    <div
      class="composer"
      role="presentation"
      data-dragging={dragging()}
      onDragOver={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragging(false);
      }}
    >
      <Show when={props.session.live.queued.length > 0}>
        <div class="composer-attachments">
          <For each={props.session.live.queued}>
            {(q, i) => (
              <div class="attachment" title={q}>
                <span class="muted">queued</span>
                <span class="truncate">{q}</span>
                <button
                  type="button"
                  class="btn"
                  data-size="icon-sm"
                  title="Remove from queue"
                  onClick={() => props.session.unqueue(i())}
                >
                  <Close size={11} />
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={pending().length > 0}>
        <div class="composer-attachments">
          <For each={pending()}>
            {(a) => (
              <div class="attachment">
                {a.kind === "image" ? <ImageIcon size={13} /> : <FileIcon size={13} />}
                <span class="truncate">{a.name}</span>
                <span class="muted tabular">{formatBytes(a.size)}</span>
                <button
                  type="button"
                  class="btn"
                  data-size="icon-sm"
                  title="Remove"
                  onClick={() => setPending((p) => p.filter((x) => x.id !== a.id))}
                >
                  <Close size={11} />
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>

      <div class="composer-field">
        <textarea
          ref={field}
          class="composer-input"
          rows={1}
          placeholder={
            busy()
              ? steerable()
                ? "Working — type to steer the run, Esc to stop"
                : "Working — messages queue for after this run, Esc to stop"
              : "Ask, or describe what to change…"
          }
          value={text()}
          spellcheck={false}
          onInput={(e) => {
            setText(e.currentTarget.value);
            autosize();
          }}
          onKeyDown={onKeyDown}
          onPaste={(e) => void onPaste(e)}
        />
      </div>

      <div class="composer-bar">
        <button
          type="button"
          class="btn"
          data-size="icon"
          title="Attach files — or paste an image straight in"
          onClick={() => void pickFiles().then(attach)}
        >
          <Attach size={15} />
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon"
          title="Sketch pad — draw a wireframe or annotate one, then send it to the agent"
          onClick={() => void import("./SketchPad").then((m) => m.openSketch())}
        >
          <Edit size={15} />
        </button>
        <PolicyControls />
        <div class="spacer" />
        <Show when={store.settings()?.lowBandwidth}>
          <span class="chip" title="Trimmed context and no background refreshes">
            low bandwidth
          </span>
        </Show>
        <EffortPicker chatId={props.chatId} />
        <GraphButton chatId={props.chatId} />
        <ModelPicker chatId={props.chatId} />
        <Show when={busy()}>
          <button
            type="button"
            class="btn"
            data-variant="danger"
            data-size="icon"
            onClick={() => void props.session.cancel()}
            title="Stop  Esc"
          >
            <Stop size={13} />
          </button>
        </Show>
        <button
          type="button"
          class="btn"
          data-variant="primary"
          data-size="icon"
          disabled={!canSend()}
          onClick={() => void send()}
          title={busy() ? (steerable() ? "Steer the run  ⏎" : "Queue for after this run  ⏎") : "Send  ⏎"}
        >
          <Send size={15} />
        </button>
      </div>

      <WorkspaceBar chatId={props.chatId} session={props.session} />
    </div>
  );
}

const MODES: [string, string, string][] = [
  ["readOnly", "read only", "Inspect and explain. Writes, commands and network calls are refused."],
  ["ask", "ask first", "Every change and command needs a click."],
  ["acceptEdits", "accept edits", "File edits apply straight away; commands still ask."],
  ["auto", "auto", "Nothing asks. Sandbox limits still apply."],
];

/**
 * Permission mode and sandbox, within reach of the message box.
 *
 * These decide what the next message is allowed to do, so they belong beside
 * where it is typed rather than three clicks away in settings.
 */
function PolicyControls() {
  const cfg = () => store.settings();
  const mode = () => cfg()?.permissionMode ?? "acceptEdits";
  const hint = () => MODES.find(([id]) => id === mode())?.[2] ?? "";

  return (
    <Show when={cfg()}>
      {(c) => (
        <>
          <label class="mini-field" title={hint()}>
            <span class="mini-label">mode</span>
            <select
              class="mini-select"
              value={mode()}
              onChange={(e) => void store.setSettings({ ...c(), permissionMode: e.currentTarget.value })}
            >
              <For each={MODES}>{([id, text]) => <option value={id}>{text}</option>}</For>
            </select>
          </label>

          <button
            type="button"
            class="btn"
            data-size="sm"
            data-on={c().sandboxFiles}
            title={
              c().sandboxFiles
                ? "File access is confined to this chat's folder"
                : "The agent can read and write anywhere your account can"
            }
            onClick={() => void store.setSettings({ ...c(), sandboxFiles: !c().sandboxFiles })}
          >
            {c().sandboxFiles ? "sandboxed" : "no sandbox"}
          </button>
        </>
      )}
    </Show>
  );
}

/**
 * The Model Execution Graph, one seat over from the model picker.
 *
 * Lit while this chat is bound to a graph — every send then flows through the
 * graph's stages instead of one bare agent loop. Click opens the canvas.
 */
function GraphButton(props: { chatId: string }) {
  const bound = () => megState.binding(props.chatId);
  const name = () => megState.graphs().find((g) => g.id === bound())?.name;
  return (
    <button
      type="button"
      class="btn"
      data-size="sm"
      data-on={!!bound()}
      title={
        bound()
          ? `Messages run through the "${name() ?? "…"}" graph — click to open  ⌘⇧E`
          : "Model Execution Graph — break a request into research, design, execution and verification  ⌘⇧E"
      }
      onClick={() => {
        if (bound() && !megState.loaded()) void megState.load();
        store.openPane({ kind: "graph" });
      }}
    >
      <NodeGraph size={13} />
      <Show when={bound() && name()}>{(n) => <span class="truncate meg-chip-name">{n()}</span>}</Show>
    </button>
  );
}

/** Used when the catalogue has not loaded, or lists no explicit levels. */
const GENERIC_EFFORTS = ["low", "medium", "high"];

/**
 * Reasoning effort.
 *
 * The levels differ by model — some offer `xhigh`/`max`, some only
 * `low`/`medium`/`high`, and many none at all — so the options come from the
 * selected model's own capability record rather than a hard-coded list. The
 * control hides entirely for a model that does not reason.
 */
function EffortPicker(props: { chatId: string }) {
  const cfg = () => store.settings();
  const modelId = () => store.chats().find((c) => c.id === props.chatId)?.model ?? cfg()?.defaultModel ?? "";
  const info = () => store.models().find((m) => m.id === modelId());

  const supported = () => {
    const r = info()?.reasoning;
    if (r) return r.supportedEfforts.length > 0 ? r.supportedEfforts : GENERIC_EFFORTS;
    // Unknown model (not in the catalogue): offer the common levels rather than
    // hiding a control the model may well support.
    if (!info()) return GENERIC_EFFORTS;
    const params = info()?.supportedParameters ?? [];
    return params.includes("reasoning") || params.includes("reasoning_effort") ? GENERIC_EFFORTS : [];
  };

  const mandatory = () => info()?.reasoning?.mandatory ?? false;

  return (
    <Show when={cfg() && supported().length > 0}>
      <label
        class="mini-field"
        title={
          mandatory()
            ? "This model always reasons; the level is adjustable"
            : "Reasoning effort for this model"
        }
      >
        <span class="mini-label">think</span>
        <select
          class="mini-select"
          value={cfg()?.reasoningEffort ?? "off"}
          onChange={(e) => {
            const c = cfg();
            if (!c) return;
            const v = e.currentTarget.value;
            void store.setSettings({ ...c, reasoningEffort: v === "off" ? null : v });
          }}
        >
          <Show when={!mandatory()}>
            <option value="off">off</option>
          </Show>
          <For each={supported()}>{(v) => <option value={v}>{v}</option>}</For>
        </select>
      </label>
    </Show>
  );
}
