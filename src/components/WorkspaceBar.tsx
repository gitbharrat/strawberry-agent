// The strip under the message box.
//
// Three facts you need before pressing send, and cannot get from the transcript:
// which folder the agent will touch, which git branch that folder is on, and
// whether a remote host is one click away. Each is also the control that
// changes it.

import { createEffect, createResource, createSignal, For, on, onCleanup, Show } from "solid-js";
import { Folder, Plus, Refresh, Server } from "./Icons";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { pickDirectory } from "~/lib/actions";
import { getSession, type Session } from "~/lib/session";
import { ContextMeter } from "./ContextMeter";
import { errText } from "~/lib/err";

export function WorkspaceBar(props: { chatId: string; session: Session }) {
  const [busy, setBusy] = createSignal(false);
  const [hostsOpen, setHostsOpen] = createSignal(false);

  const [status, { refetch }] = createResource(
    () => props.chatId,
    (id) => api.workspaceStatus(id),
  );
  const [hosts] = createResource(() => api.sshHosts());
  const [configHosts] = createResource(
    () => props.chatId,
    (id) => api.sshConfigHosts(id),
  );
  const projectHosts = () => (configHosts() ?? []).filter((h) => h.source === "project");
  const userHosts = () => (configHosts() ?? []).filter((h) => h.source === "user");

  // The branch and dirty count go stale as soon as the agent touches anything.
  createEffect(
    on(
      () => getSession(props.chatId).live.status,
      (s) => {
        if (s === "idle") void refetch();
      },
      { defer: true },
    ),
  );

  // Close when the pointer goes down anywhere OUTSIDE the ssh cluster. Two
  // traps here, both learned the hard way. stopPropagation in the menu cannot
  // guard a document-level listener, because Solid delivers onClick by
  // delegation from the document root. And the containment test must run on
  // mousedown, not click: picking a host swaps the menu to the folder step
  // synchronously, so by click time the pressed row is already detached and
  // contains() would call it an outside click.
  let sshCluster: HTMLDivElement | undefined;
  const onDocDown = (e: MouseEvent) => {
    if (sshCluster?.contains(e.target as Node)) return;
    setHostsOpen(false);
  };
  createEffect(() => {
    if (!hostsOpen()) return;
    document.addEventListener("mousedown", onDocDown);
    onCleanup(() => document.removeEventListener("mousedown", onDocDown));
  });

  async function chooseFolder() {
    const dir = await pickDirectory("Working folder for this chat");
    if (!dir) return;
    await api.updateChat({ id: props.chatId, cwd: dir });
    void store.refreshChats();
    void refetch();
  }

  async function initRepo() {
    const cwd = status()?.cwd;
    if (!cwd) return;
    setBusy(true);
    try {
      await api.gitInit(cwd);
      toast("ok", "Initialised a git repository");
      void refetch();
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setBusy(false);
    }
  }

  // Step two of the hosts menu: which folder to land in. Remembered per host.
  const [pick, setPick] = createSignal<{ kind: "host" | "alias"; value: string; label: string } | null>(null);
  const [dir, setDir] = createSignal("~");
  const dirKey = (p: { kind: string; value: string }) => `sa.sshdir:${p.kind}:${p.value}`;

  function openHost(hostId: string, label: string) {
    pickHost({ kind: "host", value: hostId, label });
  }

  function openConfigHost(alias: string) {
    pickHost({ kind: "alias", value: alias, label: alias });
  }

  function pickHost(p: { kind: "host" | "alias"; value: string; label: string }) {
    setPick(p);
    try {
      setDir(localStorage.getItem(dirKey(p)) ?? "~");
    } catch {
      setDir("~");
    }
  }

  function rememberDir(p: { kind: string; value: string }, d: string) {
    try {
      localStorage.setItem(dirKey(p), d);
    } catch {
      /* private mode */
    }
  }

  // Terminal-only escape hatch: a shell over the system ssh, workspace left
  // where it is. ProxyJump and exotic config options keep working here.
  function terminalOnly() {
    const p = pick();
    if (!p) return;
    const d = dir().trim() || "~";
    rememberDir(p, d);
    setHostsOpen(false);
    setPick(null);
    store.setPendingSsh({ kind: p.kind, value: p.value, dir: d === "~" ? undefined : d });
    store.openPane({ kind: "terminal" });
  }

  // The primary action: bind the chat's workspace to that machine AND open a
  // terminal there. One click, everything follows — tools, file browser,
  // folder label, shell.
  const [binding, setBinding] = createSignal(false);
  async function connectPicked() {
    const p = pick();
    const chatId = store.activeChatId();
    if (!p || !chatId || binding()) return;
    const d = dir().trim() || "~";
    rememberDir(p, d);
    setBinding(true);
    store.reportSsh(`connecting ${p.label}:${d}…`, "busy");
    try {
      const cwd = await api.setRemoteWorkspace(chatId, p.kind, p.value, d);
      store.reportSsh(`workspace on ${p.label} — tools and files run there`, "ok");
      setHostsOpen(false);
      setPick(null);
      void store.refreshChats();
      void refetch();
      const m = cwd.match(/^ssh:\/\/([^/]+)(\/.*)$/);
      if (m?.[1]) {
        store.setPendingSsh({ kind: "host", value: m[1], dir: m[2] });
        store.openPane({ kind: "terminal" });
      }
    } catch (e) {
      store.reportSsh(`${p.label}: ${errText(e)}`, "error");
    } finally {
      setBinding(false);
    }
  }

  // Shell-style Tab completion for the folder input, resolved on the host.
  const [comps, setComps] = createSignal<string[]>([]);
  async function completeDir() {
    const p = pick();
    const chatId = store.activeChatId();
    if (!p || !chatId) return;
    store.reportSsh(`reading ${p.label}…`, "busy");
    try {
      const list = await api.remoteCompleteDir(chatId, p.kind, p.value, dir().trim());
      store.clearSshStatus();
      if (list.length === 1) {
        setDir(`${list[0]}/`);
        setComps([]);
        return;
      }
      if (list.length > 1) {
        let lcp = list[0] ?? "";
        for (const s of list) {
          let i = 0;
          while (i < lcp.length && s[i] === lcp[i]) i++;
          lcp = lcp.slice(0, i);
        }
        if (lcp.length > dir().trim().length) setDir(lcp);
        setComps(list.slice(0, 12));
        return;
      }
      setComps([]);
    } catch (e) {
      store.reportSsh(`${p.label}: ${errText(e)}`, "error");
    }
  }

  createEffect(() => {
    if (!hostsOpen()) setPick(null);
  });

  const shortPath = () => {
    const cwd = status()?.cwd;
    if (!cwd) return "no folder";
    const remote = cwd.match(/^ssh:\/\/([^/]+)(\/.*)$/);
    if (remote) {
      const label = (hosts() ?? []).find((h) => h.id === remote[1])?.label?.replace(/^cfg:/, "");
      const tail = (remote[2] ?? "").split("/").filter(Boolean).slice(-2).join("/");
      return `${label ?? "remote"} ⇅ ${tail || "/"}`;
    }
    const home = "/Users/";
    const parts = cwd.split("/").filter(Boolean);
    const tail = parts.slice(-2).join("/");
    return cwd.startsWith(home) ? `~/${tail}` : tail || cwd;
  };

  return (
    <div class="wsbar">
      <button
        type="button"
        class="wsbar-item"
        title={status()?.cwd ?? "Choose the folder the agent works in"}
        onClick={() => void chooseFolder()}
      >
        <Folder size={11} />
        <span class="truncate">{shortPath()}</span>
      </button>

      <Show when={status()?.cwd}>
        <span class="wsbar-sep" />
        <Show
          when={status()?.isRepo}
          fallback={
            <Show
              when={status()?.gitAvailable}
              fallback={<span class="wsbar-item muted">git not installed</span>}
            >
              <button
                type="button"
                class="wsbar-item wsbar-action"
                disabled={busy()}
                title="Run git init in this folder"
                onClick={() => void initRepo()}
              >
                <Plus size={10} /> git init
              </button>
            </Show>
          }
        >
          <button
            type="button"
            class="wsbar-item"
            title={status()?.detached ? "Detached head" : "Current branch"}
            onClick={() => void refetch()}
          >
            <GitIcon />
            <span class="truncate">{status()?.branch}</span>
            <Show when={status()?.dirty}>
              {(n) => (
                <span class="wsbar-dirty" title={`${n()} uncommitted change(s)`}>
                  ●{n()}
                </span>
              )}
            </Show>
          </button>
        </Show>
      </Show>

      <span class="wsbar-sep" />

      <div class="wsbar-ssh" ref={sshCluster}>
        <button
          type="button"
          class="wsbar-item"
          title="Open a shell on a saved SSH host"
          onClick={() => setHostsOpen(!hostsOpen())}
        >
          <Server size={11} />
          <span>ssh</span>
          <Show when={(hosts() ?? []).length > 0}>
            <span class="muted">{(hosts() ?? []).length}</span>
          </Show>
        </button>

        <Show when={store.sshStatus()}>
          {(s) => (
            <button
              type="button"
              class="wsbar-status"
              data-tone={s().tone}
              title={`${s().text} — click to dismiss`}
              onClick={() => store.clearSshStatus()}
            >
              <Show when={s().tone === "busy"}>
                <span class="wsbar-status-dot" />
              </Show>
              <span class="truncate">{s().text}</span>
            </button>
          )}
        </Show>

        <Show when={hostsOpen()}>
          <div class="wsbar-menu">
            <Show when={pick()}>
              {(p) => (
                <>
                  <div class="wsbar-menu-head">folder on {p().label}</div>
                  <div class="wsbar-menu-form">
                    <input
                      class="input"
                      data-mono="true"
                      placeholder="~  (Tab completes)"
                      value={dir()}
                      ref={(el) => queueMicrotask(() => el.select())}
                      onInput={(e) => {
                        setDir(e.currentTarget.value);
                        setComps([]);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Tab") {
                          e.preventDefault();
                          void completeDir();
                        }
                        if (e.key === "Enter") void connectPicked();
                        if (e.key === "Escape") {
                          e.stopPropagation();
                          setPick(null);
                        }
                      }}
                    />
                    <button
                      type="button"
                      class="btn"
                      data-variant="primary"
                      data-size="sm"
                      disabled={binding()}
                      title="Bind this chat's workspace to that machine and open a terminal there"
                      onClick={() => void connectPicked()}
                    >
                      {binding() ? "connecting…" : "Connect"}
                    </button>
                  </div>
                  <Show when={comps().length > 0}>
                    <div class="wsbar-menu-comp">
                      <For each={comps()}>
                        {(c) => (
                          <button
                            type="button"
                            class="wsbar-menu-item"
                            onClick={() => {
                              setDir(`${c}/`);
                              setComps([]);
                            }}
                          >
                            <span class="truncate">{c}/</span>
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>
                  <button
                    type="button"
                    class="wsbar-menu-item muted"
                    title="Just a shell over your system ssh — the workspace stays where it is"
                    onClick={terminalOnly}
                  >
                    terminal only
                  </button>
                  <button type="button" class="wsbar-menu-item muted" onClick={() => setPick(null)}>
                    ← hosts
                  </button>
                </>
              )}
            </Show>
            <Show when={!pick() && projectHosts().length > 0}>
              <div class="wsbar-menu-head">this folder — .ssh/config</div>
              <For each={projectHosts()}>
                {(h) => (
                  <button type="button" class="wsbar-menu-item" onClick={() => openConfigHost(h.alias)}>
                    <Server size={11} />
                    <span class="truncate">{h.alias}</span>
                    <span class="muted truncate">
                      {h.user ? `${h.user}@` : ""}
                      {h.hostName ?? ""}
                    </span>
                  </button>
                )}
              </For>
            </Show>
            <Show when={!pick() && userHosts().length > 0}>
              <div class="wsbar-menu-head">~/.ssh/config</div>
              <For each={userHosts()}>
                {(h) => (
                  <button type="button" class="wsbar-menu-item" onClick={() => openConfigHost(h.alias)}>
                    <Server size={11} />
                    <span class="truncate">{h.alias}</span>
                    <span class="muted truncate">
                      {h.user ? `${h.user}@` : ""}
                      {h.hostName ?? ""}
                    </span>
                  </button>
                )}
              </For>
            </Show>
            <Show when={!pick() && (hosts() ?? []).length > 0}>
              <div class="wsbar-menu-head">saved in app</div>
            </Show>
            <For
              each={pick() ? [] : (hosts() ?? [])}
              fallback={
                <Show when={(configHosts() ?? []).length === 0}>
                  <div class="wsbar-menu-empty muted">
                    No hosts found — add one, or define them in ~/.ssh/config.
                  </div>
                </Show>
              }
            >
              {(h) => (
                <button type="button" class="wsbar-menu-item" onClick={() => openHost(h.id, h.label)}>
                  <Server size={11} />
                  <span class="truncate">{h.label}</span>
                  <span class="muted truncate">
                    {h.username}@{h.host}
                  </span>
                </button>
              )}
            </For>
            <button
              type="button"
              class="wsbar-menu-item"
              onClick={() => {
                setHostsOpen(false);
                store.openSettings("ssh");
              }}
            >
              <Plus size={11} /> Add a host…
            </button>
          </div>
        </Show>
      </div>

      <div class="spacer" />

      <ContextMeter session={props.session} chatId={props.chatId} />

      <button type="button" class="wsbar-item" title="Refresh branch status" onClick={() => void refetch()}>
        <Refresh size={10} />
      </button>
    </div>
  );
}

/** A branch glyph, sized to sit inline with 11px text. */
function GitIcon() {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      aria-hidden="true"
    >
      <circle cx="4.5" cy="3.5" r="1.8" />
      <circle cx="4.5" cy="12.5" r="1.8" />
      <circle cx="11.5" cy="7" r="1.8" />
      <path d="M4.5 5.3v5.4M6.3 7h3.4a1.8 1.8 0 0 1 0 0" />
      <path d="M9.7 8.4a5 5 0 0 1-5.2 2.3" />
    </svg>
  );
}
