// Terminals — local PTYs and remote SSH shells, in tabs.
//
// xterm and its WebGL renderer are a lazy chunk: a user who never opens a
// terminal never downloads or parses them. Bytes arrive from Rust already
// batched, so `write` is called about once a frame rather than once per read.

import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Close, Plus, Server, Terminal as TerminalIcon } from "./Icons";
import { api, Channel } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import type { SshHost, TerminalInfo } from "~/lib/types";
import { errText } from "~/lib/err";

/* Terminal palettes.
 *
 * Tuned to the app rather than inherited from xterm's defaults: the same warm
 * red as the accent, a green that matches the calyx, and enough separation
 * between the normal and bright ramps to read at 13px. */
const DARK_TERM = {
  background: "#0d0d0d",
  foreground: "#d6d6d6",
  cursor: "#f5304f",
  cursorAccent: "#0d0d0d",
  selectionBackground: "#2f4257",
  selectionForeground: "#ffffff",
  black: "#22222a",
  red: "#f5304f",
  green: "#46b97a",
  yellow: "#e8a33a",
  blue: "#4a7dbf",
  magenta: "#a98cf0",
  cyan: "#3fb8c4",
  white: "#c9c9c9",
  brightBlack: "#5a5a5a",
  brightRed: "#ff6a7f",
  brightGreen: "#5be38e",
  brightYellow: "#ffc464",
  brightBlue: "#6f9fd8",
  brightMagenta: "#c3aaff",
  brightCyan: "#63dbe6",
  brightWhite: "#f2f2f2",
};

const LIGHT_TERM = {
  background: "#e6e6e6",
  foreground: "#1e1e1e",
  cursor: "#e11a40",
  cursorAccent: "#e6e6e6",
  selectionBackground: "#bed3ee",
  selectionForeground: "#1e1e1e",
  black: "#3a3a3a",
  red: "#c0182f",
  green: "#16794c",
  yellow: "#8a5a08",
  blue: "#2a5fa8",
  magenta: "#6f4bc4",
  cyan: "#136b74",
  white: "#d0d0d0",
  brightBlack: "#6b6b6b",
  brightRed: "#e11a40",
  brightGreen: "#1fa85c",
  brightYellow: "#b57d14",
  brightBlue: "#4a7dbf",
  brightMagenta: "#8b64e0",
  brightCyan: "#1a8e99",
  brightWhite: "#ffffff",
};

interface Tab {
  info: TerminalInfo;
  mount: (el: HTMLDivElement) => void;
  dispose: () => void;
  focus: () => void;
  fit: () => void;
}

export function TerminalPane() {
  const [tabs, setTabs] = createSignal<Tab[]>([]);
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [hosts, setHosts] = createSignal<SshHost[]>([]);
  const [menuOpen, setMenuOpen] = createSignal(false);
  let hostEl!: HTMLDivElement;
  let stackEl!: HTMLDivElement;

  onMount(() => {
    void api
      .sshHosts()
      .then(setHosts)
      .catch(() => {});
    // A default local shell — unless this pane was opened to connect
    // somewhere, or the chat's workspace lives on a remote host, in which
    // case the first tab is a shell there.
    if (!store.pendingSsh()) {
      const remote = store.activeChat()?.cwd?.match(/^ssh:\/\/([^/]+)(\/.*)$/);
      if (remote?.[1]) store.setPendingSsh({ kind: "host", value: remote[1], dir: remote[2] });
      else void openLocal();
    }
    const onResize = () => {
      for (const t of tabs()) t.fit();
    };
    window.addEventListener("resize", onResize);
    // The stack resizes without a window resize too: the splitter drags, or
    // the whole pane is hidden and later re-shown (a "closed" tab coming
    // back). Fitting on its size changes keeps the grid true in all of them.
    const ro = new ResizeObserver(onResize);
    ro.observe(stackEl);
    onCleanup(() => {
      window.removeEventListener("resize", onResize);
      ro.disconnect();
    });
  });

  /** A cd line safe to type into any POSIX-ish remote shell. */
  function cdLine(dir: string): string {
    if (dir.startsWith("~/")) {
      const rest = dir.slice(2).replace(/(["\\$`])/g, "\\$1");
      return `cd -- "$HOME/${rest}"`;
    }
    return `cd -- '${dir.replace(/'/g, "'\\''")}'`;
  }

  // The workspace strip asks for an SSH shell without knowing whether this
  // pane exists yet; the pending signal is consumed here on mount or whenever
  // it is set while the pane is already open.
  createEffect(() => {
    const req = store.pendingSsh();
    if (!req) return;
    store.setPendingSsh(null);
    if (req.kind === "host") {
      void spawn("ssh", req.value).then((info) => {
        if (!info || !req.dir) return;
        // Typed into the shell rather than baked into the ssh command: hosts
        // that auto-exec tmux from their login shell (vast.ai containers do)
        // replace whatever the command cd'd into — typing lands in the final
        // interactive shell. The delay lets banners and tmux settle.
        setTimeout(() => void api.termWrite(info.id, `${cdLine(req.dir ?? "~")}\r`).catch(() => {}), 1200);
      });
      return;
    }
    // Config-alias hosts connect through the system ssh in a local shell, so
    // keys, agents and ProxyJump behave exactly as in a terminal. The cd is
    // typed after the connection settles, for the same auto-tmux reason as
    // above.
    void spawn("local").then((info) => {
      if (!info) return;
      store.reportSsh(`terminal → ssh ${req.value}`, "ok");
      const safe = req.value.replace(/'/g, "'\\''");
      setTimeout(() => void api.termWrite(info.id, `ssh '${safe}'\r`).catch(() => {}), 180);
      if (req.dir) {
        setTimeout(() => void api.termWrite(info.id, `${cdLine(req.dir ?? "~")}\r`).catch(() => {}), 2600);
      }
    });
  });

  onCleanup(() => {
    for (const t of tabs()) {
      t.dispose();
      void api.termClose(t.info.id).catch(() => {});
    }
  });

  async function spawn(kind: "local" | "ssh", hostId?: string) {
    const { Terminal } = await import("@xterm/xterm");
    const { FitAddon } = await import("@xterm/addon-fit");
    await import("@xterm/xterm/css/xterm.css");

    const settings = store.settings();
    const dark = document.documentElement.dataset.theme === "dark";
    const term = new Terminal({
      fontFamily: settings?.terminalFontFamily ?? "monospace",
      fontSize: settings?.terminalFontSize ?? 12,
      lineHeight: 1.15,
      scrollback: settings?.terminalScrollback ?? 5000,
      cursorBlink: true,
      cursorStyle: "bar",
      allowProposedApi: true,
      // Rust already coalesces, so a small render budget keeps latency low.
      smoothScrollDuration: 0,
      theme: dark ? DARK_TERM : LIGHT_TERM,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);

    const decoder = new TextDecoder();
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (payload) => {
      const bytes =
        payload instanceof ArrayBuffer
          ? new Uint8Array(payload)
          : new Uint8Array(payload as unknown as number[]);
      // A zero-length frame is the backend's end-of-stream marker.
      if (bytes.length === 0) {
        term.write("\r\n\x1b[2m[process exited]\x1b[0m\r\n");
        return;
      }
      term.write(decoder.decode(bytes, { stream: true }));
    };

    const hostLabel = () =>
      (hosts() ?? []).find((h) => h.id === hostId)?.label?.replace(/^cfg:/, "") ?? hostId ?? "host";
    // A remote-workspace cwd is an address for the tools, not a directory a
    // local pty can start in.
    const rawCwd = store.activeChat()?.cwd ?? undefined;
    const localCwd = rawCwd?.startsWith("ssh://") ? undefined : rawCwd;
    let info: TerminalInfo;
    try {
      if (kind === "ssh") store.reportSsh(`connecting ${hostLabel()}…`, "busy");
      info =
        kind === "local"
          ? await api.termOpenLocal({ cwd: localCwd, cols: 80, rows: 24 }, channel)
          : await api.termOpenSsh({ hostId: hostId!, cols: 80, rows: 24 }, channel);
      if (kind === "ssh") store.reportSsh(`connected ${hostLabel()}`, "ok");
    } catch (e) {
      term.dispose();
      if (kind === "ssh") store.reportSsh(`${hostLabel()}: ${errText(e)}`, "error");
      else toast("error", errText(e));
      return null;
    }

    term.onData((data) => void api.termWrite(info.id, data).catch(() => {}));
    term.onResize(({ cols, rows }) => void api.termResize(info.id, cols, rows).catch(() => {}));

    let container: HTMLDivElement | null = null;
    const tab: Tab = {
      info,
      mount(el) {
        if (container === el) return;
        container = el;
        term.open(el);
        // WebGL is a large win on scroll-heavy output but is not available
        // everywhere; the canvas fallback is built in.
        void import("@xterm/addon-webgl")
          .then(({ WebglAddon }) => {
            const addon = new WebglAddon();
            addon.onContextLoss(() => addon.dispose());
            term.loadAddon(addon);
          })
          .catch(() => {});
        requestAnimationFrame(() => {
          fit.fit();
          term.focus();
        });
      },
      dispose() {
        term.dispose();
      },
      focus() {
        term.focus();
      },
      fit() {
        try {
          fit.fit();
        } catch {
          /* not visible */
        }
      },
    };

    setTabs((t) => [...t, tab]);
    setActiveId(info.id);
    return info;
  }

  const openLocal = () => spawn("local");

  async function close(id: string) {
    const tab = tabs().find((t) => t.info.id === id);
    tab?.dispose();
    await api.termClose(id).catch(() => {});
    setTabs((t) => t.filter((x) => x.info.id !== id));
    if (activeId() === id) setActiveId(tabs()[0]?.info.id ?? null);
  }

  return (
    <div class="term">
      <div class="term-tabs">
        <For each={tabs()}>
          {(t) => (
            <div class="term-tab" data-active={t.info.id === activeId()}>
              <button
                type="button"
                class="term-tab-main"
                onClick={() => {
                  setActiveId(t.info.id);
                  queueMicrotask(() => {
                    t.fit();
                    t.focus();
                  });
                }}
              >
                {t.info.kind === "ssh" ? <Server size={12} /> : <TerminalIcon size={12} />}
                <span class="truncate">{t.info.title}</span>
              </button>
              <button
                type="button"
                class="term-close"
                aria-label={`Close ${t.info.title}`}
                onClick={() => void close(t.info.id)}
              >
                <Close size={10} />
              </button>
            </div>
          )}
        </For>

        <div class="term-new" ref={hostEl}>
          <button
            type="button"
            class="btn"
            data-size="icon-sm"
            title="New terminal"
            onClick={() => void openLocal()}
          >
            <Plus size={12} />
          </button>
          <Show when={hosts().length > 0}>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="Connect over SSH"
              onClick={() => setMenuOpen(!menuOpen())}
            >
              <Server size={12} />
            </button>
          </Show>
          <Show when={menuOpen()}>
            <div class="term-hosts" role="menu" onMouseLeave={() => setMenuOpen(false)}>
              <For each={hosts()}>
                {(h) => (
                  <button
                    type="button"
                    class="term-host"
                    role="menuitem"
                    onClick={() => {
                      setMenuOpen(false);
                      void spawn("ssh", h.id);
                    }}
                  >
                    <Server size={12} />
                    <span class="truncate">{h.label}</span>
                    <span class="muted truncate">
                      {h.username}@{h.host}
                    </span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>

      <div class="term-stack" ref={stackEl}>
        <For each={tabs()}>
          {(t) => (
            <div
              class="term-surface"
              data-active={t.info.id === activeId()}
              ref={(el) => queueMicrotask(() => t.mount(el))}
            />
          )}
        </For>
        <Show when={tabs().length === 0}>
          <div class="empty">
            <p>No terminals open.</p>
          </div>
        </Show>
      </div>
    </div>
  );
}
