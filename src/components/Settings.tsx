// Settings.
//
// A full-height sheet rather than a modal, grouped by the question being
// answered — how it looks, which model, what the agent may do, how it talks to
// the network. Every control writes through immediately; there is no Save
// button to forget.

import { createResource, createSignal, For, type JSX, Show } from "solid-js";
import { Check, Close, Key, Refresh, Server, Trash, Warning } from "./Icons";
import { Logo } from "./Logo";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { formatCost } from "~/lib/actions";
import type { Settings as S, SshHost } from "~/lib/types";
import { errText } from "~/lib/err";

type Tab = "general" | "models" | "agent" | "network" | "ssh" | "about";

const TABS: [Tab, string][] = [
  ["general", "Appearance"],
  ["models", "Models"],
  ["agent", "Agent"],
  ["network", "Network"],
  ["ssh", "SSH"],
  ["about", "About"],
];

export function SettingsSheet() {
  // The sheet opens on whatever tab asked for it — the ssh menu's "Add host…"
  // lands straight on SSH instead of making the user hunt.
  const [tab, setTab] = createSignal<Tab>((store.settingsTab() as Tab) ?? "general");
  const s = () => store.settings();
  const patch = (p: Partial<S>) => {
    const cur = s();
    if (cur) void store.setSettings({ ...cur, ...p });
  };

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: modal backdrop; Escape closes it and every control inside is focusable
    <div
      class="scrim"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) store.setSettingsOpen(false);
      }}
    >
      <div class="sheet" role="dialog" aria-label="Settings">
        <header class="sheet-head" data-tauri-drag-region>
          <Logo size={16} />
          <strong>Settings</strong>
          <span class="muted">— every change saves immediately</span>
          <div class="spacer" />
          <button
            type="button"
            class="btn"
            data-size="icon"
            onClick={() => store.setSettingsOpen(false)}
            title="Close  Esc"
          >
            <Close size={15} />
          </button>
        </header>

        <div class="sheet-body">
          <nav class="sheet-tabs">
            <For each={TABS}>
              {([id, label]) => (
                <button type="button" class="sheet-tab" data-active={tab() === id} onClick={() => setTab(id)}>
                  {label}
                </button>
              )}
            </For>
          </nav>

          <div class="sheet-content">
            <Show when={s()}>
              {(cfg) => (
                <>
                  <Show when={tab() === "general"}>
                    <General cfg={cfg()} patch={patch} />
                  </Show>
                  <Show when={tab() === "models"}>
                    <Models cfg={cfg()} patch={patch} />
                  </Show>
                  <Show when={tab() === "agent"}>
                    <Agent cfg={cfg()} patch={patch} />
                  </Show>
                  <Show when={tab() === "network"}>
                    <Network cfg={cfg()} patch={patch} />
                  </Show>
                  <Show when={tab() === "ssh"}>
                    <Ssh />
                  </Show>
                  <Show when={tab() === "about"}>
                    <About />
                  </Show>
                </>
              )}
            </Show>
          </div>
        </div>
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ sections

interface SectionProps {
  cfg: S;
  patch: (p: Partial<S>) => void;
}

function Field(props: { label: string; hint?: string; children: JSX.Element }) {
  return (
    // biome-ignore lint/a11y/noLabelWithoutControl: the control is supplied as children
    <label class="field">
      <div class="field-label">
        <span>{props.label}</span>
        <Show when={props.hint}>{(h) => <span class="field-hint muted">{h()}</span>}</Show>
      </div>
      <div class="field-control">{props.children}</div>
    </label>
  );
}

function Toggle(props: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string }) {
  return (
    <label class="switch-row">
      <input
        type="checkbox"
        checked={props.checked}
        onChange={(e) => props.onChange(e.currentTarget.checked)}
      />
      <div>
        <div>{props.label}</div>
        <Show when={props.hint}>{(h) => <div class="muted field-hint">{h()}</div>}</Show>
      </div>
    </label>
  );
}

function General(p: SectionProps) {
  return (
    <>
      <h3>Appearance</h3>
      <Field label="Theme">
        <div class="segmented">
          <For each={["system", "light", "dark"] as const}>
            {(t) => (
              <button type="button" data-active={p.cfg.theme === t} onClick={() => p.patch({ theme: t })}>
                {t}
              </button>
            )}
          </For>
        </div>
      </Field>
      <Field label="Density" hint="Compact tightens spacing without shrinking text">
        <div class="segmented">
          <For each={["comfortable", "compact"] as const}>
            {(d) => (
              <button type="button" data-active={p.cfg.density === d} onClick={() => p.patch({ density: d })}>
                {d}
              </button>
            )}
          </For>
        </div>
      </Field>
      <Field label="Interface text size">
        <input
          class="input"
          type="number"
          min="11"
          max="20"
          value={p.cfg.uiFontSize}
          onChange={(e) => p.patch({ uiFontSize: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field label="Content width" hint="Max reading width of the chat, px — margins grow around it">
        <input
          class="input"
          type="number"
          min="560"
          max="1600"
          step="20"
          value={p.cfg.contentWidth}
          onChange={(e) => p.patch({ contentWidth: Number(e.currentTarget.value) || 820 })}
        />
      </Field>
      <Toggle
        label="Reduce motion"
        hint="Removes transitions and the run-bar pulse"
        checked={p.cfg.reduceMotion}
        onChange={(v) => p.patch({ reduceMotion: v })}
      />

      <div class="divider" />
      <h3>Terminal</h3>
      <Field label="Shell" hint="Leave blank to use $SHELL">
        <input
          class="input"
          data-mono="true"
          placeholder="/bin/zsh"
          value={p.cfg.shell ?? ""}
          onChange={(e) => p.patch({ shell: e.currentTarget.value || null })}
        />
      </Field>
      <Field label="Font size">
        <input
          class="input"
          type="number"
          min="9"
          max="24"
          value={p.cfg.terminalFontSize}
          onChange={(e) => p.patch({ terminalFontSize: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field label="Scrollback lines">
        <input
          class="input"
          type="number"
          min="500"
          max="200000"
          step="500"
          value={p.cfg.terminalScrollback}
          onChange={(e) => p.patch({ terminalScrollback: Number(e.currentTarget.value) })}
        />
      </Field>
    </>
  );
}

function Models(p: SectionProps) {
  const [key, setKey] = createSignal("");
  const [checking, setChecking] = createSignal(false);
  const [info, setInfo] = createSignal<Record<string, unknown> | null>(null);

  async function saveKey() {
    const k = key().trim();
    if (!k) return;
    try {
      const status = await api.setApiKey(k);
      store.setKeyStatus(status);
      setKey("");
      toast("ok", "API key saved to the system keychain");
      void store.refreshModels(true);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function verify() {
    setChecking(true);
    try {
      setInfo(await api.verifyApiKey());
      toast("ok", "Key works");
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setChecking(false);
    }
  }

  const data = () => (info()?.data ?? info()) as Record<string, unknown> | undefined;

  return (
    <>
      <h3>OpenRouter key</h3>
      <p class="muted section-note">
        Stored in the operating system keychain, never in a file or the database. It is sent only to
        openrouter.ai.
      </p>

      <Show when={store.keyStatus().configured}>
        <div class="key-status">
          <Key size={14} />
          <code>{store.keyStatus().masked}</code>
          <div class="spacer" />
          <button
            type="button"
            class="btn"
            data-size="sm"
            disabled={checking()}
            onClick={() => void verify()}
          >
            {checking() ? "Checking…" : "Test"}
          </button>
          <button
            type="button"
            class="btn"
            data-size="sm"
            data-variant="danger"
            onClick={() =>
              void api.clearApiKey().then(() => {
                void store.refreshKeyStatus();
                toast("ok", "Key removed");
              })
            }
          >
            Remove
          </button>
        </div>
      </Show>

      <Field label={store.keyStatus().configured ? "Replace key" : "API key"} hint="sk-or-v1-…">
        <div class="row-inline">
          <input
            class="input"
            data-mono="true"
            type="password"
            placeholder="sk-or-v1-…"
            value={key()}
            onInput={(e) => setKey(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void saveKey();
            }}
          />
          <button
            type="button"
            class="btn"
            data-variant="primary"
            disabled={!key().trim()}
            onClick={() => void saveKey()}
          >
            <Check size={13} /> Save
          </button>
        </div>
      </Field>

      <Show when={data()}>
        {(d) => (
          <div class="key-info">
            <Show when={d().label}>{(l) => <span class="chip">{String(l())}</span>}</Show>
            <Show when={typeof d().usage === "number"}>
              <span class="chip tabular">used {formatCost(Number(d().usage))}</span>
            </Show>
            <Show when={typeof d().limit === "number"}>
              <span class="chip tabular">limit {formatCost(Number(d().limit))}</span>
            </Show>
            <Show when={d().is_free_tier === true}>
              <span class="chip">free tier</span>
            </Show>
          </div>
        )}
      </Show>

      <div class="divider" />
      <h3>Defaults</h3>
      <Field label="Default model" hint="Used for new chats">
        <input
          class="input"
          data-mono="true"
          list="sa-models"
          value={p.cfg.defaultModel}
          onChange={(e) => p.patch({ defaultModel: e.currentTarget.value })}
        />
        <datalist id="sa-models">
          <For each={store.models()}>{(m) => <option value={m.id}>{m.name}</option>}</For>
        </datalist>
      </Field>
      <Field label="Fallback models" hint="Comma separated. Tried in order if the primary is unavailable.">
        <input
          class="input"
          data-mono="true"
          placeholder="anthropic/claude-sonnet-5, openai/gpt-5"
          value={p.cfg.fallbackModels.join(", ")}
          onChange={(e) =>
            p.patch({
              fallbackModels: e.currentTarget.value
                .split(",")
                .map((x) => x.trim())
                .filter(Boolean),
            })
          }
        />
      </Field>
      <Field label="Temperature" hint="Blank uses the model's own default">
        <input
          class="input"
          type="number"
          step="0.05"
          min="0"
          max="2"
          value={p.cfg.temperature ?? ""}
          onChange={(e) =>
            p.patch({ temperature: e.currentTarget.value === "" ? null : Number(e.currentTarget.value) })
          }
        />
      </Field>
      <Field label="Max output tokens" hint="Blank lets the provider decide">
        <input
          class="input"
          type="number"
          min="1"
          value={p.cfg.maxTokens ?? ""}
          onChange={(e) =>
            p.patch({ maxTokens: e.currentTarget.value === "" ? null : Number(e.currentTarget.value) })
          }
        />
      </Field>
      <Field label="Reasoning effort" hint="Only used by models that support it">
        <select
          class="select"
          value={p.cfg.reasoningEffort ?? ""}
          onChange={(e) => p.patch({ reasoningEffort: e.currentTarget.value || null })}
        >
          <option value="">off</option>
          <For each={["low", "medium", "high", "xhigh", "max"]}>{(v) => <option value={v}>{v}</option>}</For>
        </select>
      </Field>
      <Toggle
        label="Show reasoning"
        hint="Collapsed by default in the transcript"
        checked={p.cfg.showReasoning}
        onChange={(v) => p.patch({ showReasoning: v })}
      />
      <Field label="System prompt" hint="Prepended to every chat, on top of the built-in one">
        <textarea
          class="textarea"
          rows={5}
          placeholder="e.g. Always use British spelling. Prefer standard library over dependencies."
          value={p.cfg.systemPrompt ?? ""}
          onChange={(e) => p.patch({ systemPrompt: e.currentTarget.value || null })}
        />
      </Field>
    </>
  );
}

const MODES: [string, string, string][] = [
  ["readOnly", "Read only", "Inspect and explain. Writes, commands and network calls are refused."],
  ["ask", "Ask first", "Every change and command needs a click."],
  ["acceptEdits", "Accept edits", "File edits apply straight away; commands and network calls still ask."],
  ["auto", "Auto", "Nothing asks automatically — the model itself asks before genuinely destructive steps."],
];

function Agent(p: SectionProps) {
  const [root, setRoot] = createSignal("");

  return (
    <>
      <h3>Permissions</h3>
      <p class="muted section-note">
        Everything the agent writes is journalled, so any edit can be reverted from the changed-files panel
        regardless of the mode.
      </p>
      <div class="modes">
        <For each={MODES}>
          {([id, label, desc]) => (
            <button
              type="button"
              class="mode"
              data-active={p.cfg.permissionMode === id}
              onClick={() => p.patch({ permissionMode: id })}
            >
              <span class="mode-dot" />
              <div>
                <div class="mode-label">{label}</div>
                <div class="mode-desc muted">{desc}</div>
              </div>
            </button>
          )}
        </For>
      </div>

      <div class="divider" />
      <h3>Sandbox</h3>
      <Toggle
        label="Confine file access to the workspace"
        hint="The agent can only read and write inside the chat's working folder plus the extra roots below."
        checked={p.cfg.sandboxFiles}
        onChange={(v) => p.patch({ sandboxFiles: v })}
      />
      <Show when={!p.cfg.sandboxFiles}>
        <div class="callout" data-tone="warn">
          <Warning size={14} />
          <span>With the sandbox off, the agent can read and write anywhere your user account can.</span>
        </div>
      </Show>
      <Toggle
        label="Confine commands too"
        hint="macOS only, best effort: wraps shell commands so they cannot write outside the workspace."
        checked={p.cfg.sandboxCommands}
        onChange={(v) => p.patch({ sandboxCommands: v })}
      />

      <Field label="Extra workspace roots" hint="Added to every chat's sandbox">
        <div class="row-inline">
          <input
            class="input"
            data-mono="true"
            placeholder="/Users/you/code"
            value={root()}
            onInput={(e) => setRoot(e.currentTarget.value)}
          />
          <button
            type="button"
            class="btn"
            onClick={() => {
              const v = root().trim();
              if (!v) return;
              p.patch({ allowedRoots: [...p.cfg.allowedRoots, v] });
              setRoot("");
            }}
          >
            Add
          </button>
        </div>
      </Field>
      <div class="tag-list">
        <For each={p.cfg.allowedRoots}>
          {(r) => (
            <span class="chip">
              <code>{r}</code>
              <button
                type="button"
                onClick={() => p.patch({ allowedRoots: p.cfg.allowedRoots.filter((x) => x !== r) })}
              >
                <Close size={10} />
              </button>
            </span>
          )}
        </For>
      </div>

      <Field
        label="Never accessible"
        hint="Credential stores — SSH keys, cloud tokens, kube/docker logins, keychains. Locked in every mode so a confused or prompt-injected model can never read or leak them. The app itself still uses your SSH keys normally, and the agent still runs ssh/docker/kubectl as commands — only direct file reads of the secrets are refused."
      >
        <div class="tag-list">
          <For each={p.cfg.deniedPaths}>
            {(d) => (
              <span class="chip">
                <code>{d}</code>
              </span>
            )}
          </For>
        </div>
      </Field>

      <div class="divider" />
      <h3>Tools</h3>
      <Toggle
        label="Enable tools"
        hint="Turning this off makes the model answer from the conversation alone"
        checked={p.cfg.toolsEnabled}
        onChange={(v) => p.patch({ toolsEnabled: v })}
      />
      <div class="tool-grid">
        <For each={store.tools()}>
          {(t) => {
            const on = () => p.cfg.enabledTools.length === 0 || p.cfg.enabledTools.includes(t.name);
            return (
              <label class="tool-item" data-on={on()}>
                <input
                  type="checkbox"
                  checked={on()}
                  onChange={(e) => {
                    const all = store.tools().map((x) => x.name);
                    const current = p.cfg.enabledTools.length === 0 ? all : p.cfg.enabledTools;
                    const next = e.currentTarget.checked
                      ? [...new Set([...current, t.name])]
                      : current.filter((n) => n !== t.name);
                    p.patch({ enabledTools: next.length === all.length ? [] : next });
                  }}
                />
                <div>
                  <div class="tool-item-name">
                    <code>{t.name}</code>
                    <span class="chip" data-tone={t.risk === "read" ? undefined : "warn"}>
                      {t.risk}
                    </span>
                  </div>
                  <div class="muted field-hint">{t.description.split(".")[0]}.</div>
                </div>
              </label>
            );
          }}
        </For>
      </div>

      <Field label="Maximum steps per turn" hint="How many tool rounds one message may take">
        <input
          class="input"
          type="number"
          min="1"
          max="500"
          value={p.cfg.maxSteps}
          onChange={(e) => p.patch({ maxSteps: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field label="Tool timeout" hint="Milliseconds before a single tool is abandoned">
        <input
          class="input"
          type="number"
          min="1000"
          step="1000"
          value={p.cfg.toolTimeoutMs}
          onChange={(e) => p.patch({ toolTimeoutMs: Number(e.currentTarget.value) })}
        />
      </Field>

      <div class="divider" />
      <h3>Context</h3>
      <p class="muted section-note">
        When a run's prompt crosses the threshold, older history folds into a rolling structured summary and
        the recent tail stays verbatim. The same compaction is available any time from the context meter, the
        palette, or the model's own compact_context tool.
      </p>
      <Field label="Auto-compact at" hint="% of the model's context window. 0 disables auto-compaction">
        <input
          class="input"
          type="number"
          min="0"
          max="95"
          step="5"
          value={p.cfg.autoCompactPct}
          onChange={(e) =>
            p.patch({ autoCompactPct: Math.max(0, Math.min(95, Number(e.currentTarget.value) || 0)) })
          }
        />
      </Field>
    </>
  );
}

function Network(p: SectionProps) {
  return (
    <>
      <h3>Connection</h3>
      <Toggle
        label="Low-bandwidth mode"
        hint="Sends less history, skips background catalogue refreshes, and serves the cached model list."
        checked={p.cfg.lowBandwidth}
        onChange={(v) => p.patch({ lowBandwidth: v })}
      />
      <Field label="Connect timeout" hint="Milliseconds to establish a connection">
        <input
          class="input"
          type="number"
          min="1000"
          step="500"
          value={p.cfg.connectTimeoutMs}
          onChange={(e) => p.patch({ connectTimeoutMs: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field label="Read timeout" hint="Milliseconds without data before a request is abandoned">
        <input
          class="input"
          type="number"
          min="10000"
          step="5000"
          value={p.cfg.requestTimeoutMs}
          onChange={(e) => p.patch({ requestTimeoutMs: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field label="Retries" hint="Only before the first token; a partly written answer is never restarted">
        <input
          class="input"
          type="number"
          min="0"
          max="10"
          value={p.cfg.maxRetries}
          onChange={(e) => p.patch({ maxRetries: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field
        label="History limit"
        hint="Messages sent per request. Trimming happens at a tool-call boundary."
      >
        <input
          class="input"
          type="number"
          min="10"
          max="2000"
          value={p.cfg.historyLimit}
          onChange={(e) => p.patch({ historyLimit: Number(e.currentTarget.value) })}
        />
      </Field>
      <Field
        label="Stream batching"
        hint="Milliseconds of tokens coalesced per UI update. Higher costs less CPU."
      >
        <input
          class="input"
          type="number"
          min="0"
          max="120"
          value={p.cfg.streamCoalesceMs}
          onChange={(e) => p.patch({ streamCoalesceMs: Number(e.currentTarget.value) })}
        />
      </Field>
      <Toggle
        label="Send app attribution"
        hint="Adds this app's name to OpenRouter request headers for their public leaderboard."
        checked={p.cfg.sendAttribution}
        onChange={(v) => p.patch({ sendAttribution: v })}
      />
      <Field label="Attachment size limit" hint="Megabytes">
        <input
          class="input"
          type="number"
          min="1"
          max="512"
          value={p.cfg.maxUploadMb}
          onChange={(e) => p.patch({ maxUploadMb: Number(e.currentTarget.value) })}
        />
      </Field>
    </>
  );
}

const BLANK_HOST: Partial<SshHost> = {
  label: "",
  host: "",
  port: 22,
  username: "",
  auth: "key",
  keyPath: null,
};

function Ssh() {
  const [hosts, { refetch }] = createResource(() => api.sshHosts());
  const [draft, setDraft] = createSignal<Partial<SshHost> | null>(null);
  const [secret, setSecret] = createSignal("");
  const [testing, setTesting] = createSignal<string | null>(null);

  async function save() {
    const d = draft();
    if (!d?.host || !d.username) {
      toast("error", "Host and username are required");
      return;
    }
    try {
      await api.saveSshHost({ ...d, label: d.label || d.host }, secret() || undefined);
      setDraft(null);
      setSecret("");
      void refetch();
      toast("ok", "Host saved");
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function test(id: string) {
    setTesting(id);
    try {
      const fp = await api.testSshHost(id);
      toast("ok", fp ? `Connected — host key ${fp.slice(0, 24)}…` : "Connected");
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setTesting(null);
    }
  }

  return (
    <>
      <h3>SSH hosts</h3>
      <p class="muted section-note">
        Passwords and key passphrases go to the system keychain. Host keys are pinned on first connect; a
        later mismatch is refused, exactly like OpenSSH.
      </p>

      <For each={hosts() ?? []} fallback={<div class="muted section-note">No hosts yet.</div>}>
        {(h) => (
          <div class="host-row">
            <Server size={14} />
            <div class="truncate">
              <div class="truncate">
                <strong>{h.label}</strong>
              </div>
              <div class="muted truncate">
                {h.username}@{h.host}:{h.port} · {h.auth}
              </div>
            </div>
            <div class="spacer" />
            <button
              type="button"
              class="btn"
              data-size="sm"
              disabled={testing() === h.id}
              onClick={() => void test(h.id)}
            >
              {testing() === h.id ? "…" : "Test"}
            </button>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="Forget pinned host key"
              onClick={() => void api.forgetHostKey(h.id).then(() => toast("ok", "Host key forgotten"))}
            >
              <Refresh size={12} />
            </button>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              data-variant="danger"
              title="Delete"
              onClick={() => void api.deleteSshHost(h.id).then(() => refetch())}
            >
              <Trash size={12} />
            </button>
          </div>
        )}
      </For>

      <Show
        when={draft()}
        fallback={
          <button
            type="button"
            class="btn"
            data-variant="outline"
            onClick={() => setDraft({ ...BLANK_HOST })}
          >
            Add host
          </button>
        }
      >
        {(d) => (
          <div class="host-form">
            <Field label="Label">
              <input
                class="input"
                value={d().label ?? ""}
                onInput={(e) => setDraft({ ...d(), label: e.currentTarget.value })}
              />
            </Field>
            <Field label="Hostname">
              <input
                class="input"
                data-mono="true"
                value={d().host ?? ""}
                onInput={(e) => setDraft({ ...d(), host: e.currentTarget.value })}
              />
            </Field>
            <Field label="Port">
              <input
                class="input"
                type="number"
                value={d().port ?? 22}
                onInput={(e) => setDraft({ ...d(), port: Number(e.currentTarget.value) })}
              />
            </Field>
            <Field label="Username">
              <input
                class="input"
                data-mono="true"
                value={d().username ?? ""}
                onInput={(e) => setDraft({ ...d(), username: e.currentTarget.value })}
              />
            </Field>
            <Field label="Authentication">
              <div class="segmented">
                <For each={["key", "password"] as const}>
                  {(a) => (
                    <button
                      type="button"
                      data-active={d().auth === a}
                      onClick={() => setDraft({ ...d(), auth: a })}
                    >
                      {a}
                    </button>
                  )}
                </For>
              </div>
            </Field>
            <Show when={d().auth === "key"}>
              <Field label="Private key path" hint="Blank tries ~/.ssh/id_ed25519, id_ecdsa, id_rsa">
                <input
                  class="input"
                  data-mono="true"
                  placeholder="~/.ssh/id_ed25519"
                  value={d().keyPath ?? ""}
                  onInput={(e) => setDraft({ ...d(), keyPath: e.currentTarget.value || null })}
                />
              </Field>
            </Show>
            <Field label={d().auth === "password" ? "Password" : "Key passphrase (optional)"}>
              <input
                class="input"
                type="password"
                value={secret()}
                onInput={(e) => setSecret(e.currentTarget.value)}
              />
            </Field>
            <div class="row-inline">
              <button type="button" class="btn" data-variant="primary" onClick={() => void save()}>
                <Check size={13} /> Save host
              </button>
              <button
                type="button"
                class="btn"
                onClick={() => {
                  setDraft(null);
                  setSecret("");
                }}
              >
                Cancel
              </button>
            </div>
          </div>
        )}
      </Show>
    </>
  );
}

function About() {
  const info = () => store.appInfo();
  return (
    <div class="about">
      <Logo size={56} />
      <h3>Strawberry Agent</h3>
      <p class="muted">A fast, bloat-free coding harness for OpenRouter models.</p>
      <Show when={info()}>
        {(i) => (
          <dl class="about-grid">
            <dt>Version</dt>
            <dd>{i().version}</dd>
            <dt>Platform</dt>
            <dd>
              {i().platform} · {i().arch}
            </dd>
            <dt>Data</dt>
            <dd class="selectable">
              <code>{i().dataDir}</code>
            </dd>
            <dt>Models cached</dt>
            <dd>{store.models().length}</dd>
          </dl>
        )}
      </Show>
    </div>
  );
}
