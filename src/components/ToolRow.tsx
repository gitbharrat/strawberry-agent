// One line per tool call.
//
// Most harnesses dump raw JSON into the transcript, which buries the answer.
// A tool call is really one fact — "it read this file", "it ran this command" —
// so it gets one line, with the detail one click away. When a call needs
// approval, the prompt appears in place rather than in a separate panel: the
// decision belongs next to the thing being decided.

import { createSignal, Match, Show, Switch } from "solid-js";
import {
  Check,
  Chevron,
  Code,
  Diff,
  File as FileIcon,
  Folder,
  Globe,
  Image as ImageIcon,
  Search,
  Server,
  Terminal,
  Warning,
} from "./Icons";
import type { ToolActivity } from "~/lib/session";
import { formatDuration, openPath } from "~/lib/actions";
import { assetUrl } from "./Viewers";
import { highlight } from "~/lib/highlight";

/** Best-effort parse: arguments stream in, so they are often incomplete. */
function parseArgs(raw: string): Record<string, unknown> {
  if (!raw) return {};
  try {
    return JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return {};
  }
}

const str = (v: unknown) => (typeof v === "string" ? v : undefined);

/** Targets for the remote-machine families (ssh transfer, docker) and the
 * web/git tool groups, split out of the main switch for readability. */
function describeRemoteTool(name: string, a: Record<string, unknown>): string | null {
  switch (name) {
    case "web_search":
      return str(a.query) ?? "";
    case "web_screenshot": {
      const target = str(a.url) ?? str(a.path) ?? "";
      const width = typeof a.width === "number" ? ` @${a.width}px` : "";
      return `${target}${width}`;
    }
    case "deep_research": {
      const q = str(a.question) ?? "";
      return q.length > 90 ? `${q.slice(0, 90)}…` : q;
    }
    case "download_file":
      return str(a.url) ?? "";
    case "git_status":
      return "workspace";
    case "git_diff":
      return str(a.path) ?? str(a.base) ?? "changes";
    case "git_log":
      return str(a.path) ?? "history";
    case "git_commit": {
      const m = str(a.message) ?? "";
      return m.length > 80 ? `${m.slice(0, 80)}…` : m;
    }
    case "git_branch":
      return str(a.create) ?? str(a.switch) ?? "branches";
    case "ssh_download":
      return str(a.remote_path) ?? "";
    case "ssh_upload":
      return str(a.local_path) ?? "";
    case "docker_ps":
      return "containers";
    case "docker_logs":
    case "docker_inspect":
      return str(a.container) ?? str(a.target) ?? "";
    case "docker_exec":
      return `${str(a.container) ?? ""}: ${str(a.command) ?? ""}`;
    default:
      return null;
  }
}

/** The single line of text that stands in for the whole call. */
export function describeTool(name: string, raw: string): string {
  const a = parseArgs(raw);
  const remote = describeRemoteTool(name, a);
  if (remote !== null) return remote;
  switch (name) {
    case "read_file":
    case "write_file":
    case "edit_file":
      return str(a.path) ?? "a file";
    case "list_dir":
      return str(a.path) ?? ".";
    case "glob":
      return str(a.pattern) ?? "*";
    case "grep": {
      const pattern = str(a.pattern) ?? "";
      const where = str(a.path);
      return where ? `${pattern}  in ${where}` : pattern;
    }
    case "run_command":
    case "ssh_exec":
      return str(a.command) ?? "";
    case "share_image":
    case "view_image": {
      const path = str(a.path) ?? "";
      return path.split("/").pop() ?? path;
    }
    case "compact_context":
      return "older history into a summary";
    case "memory_search":
      return str(a.query) ?? "";
    case "memory_read_chat":
      return str(a.chat_id) ?? "";
    case "fetch_url":
      return str(a.url) ?? "";
    case "list_ssh_hosts":
      return "configured hosts";
    case "plan":
      return str(a.title) ?? "updated the plan";
    case "ask_user":
      return str(a.question) ?? "";
    case "subagent": {
      const t = str(a.task) ?? "";
      return t.length > 90 ? `${t.slice(0, 90)}…` : t;
    }
    case "shell_start":
      return str(a.command) ?? "";
    case "shell_read":
    case "shell_send":
    case "shell_kill":
      return str(a.session_id) ?? "";
    default:
      return raw.length > 90 ? `${raw.slice(0, 90)}…` : raw;
  }
}

const VERBS: Record<string, string> = {
  read_file: "Read",
  write_file: "Wrote",
  edit_file: "Edited",
  list_dir: "Listed",
  glob: "Found files",
  grep: "Searched",
  run_command: "Ran",
  ssh_exec: "Ran remotely",
  fetch_url: "Fetched",
  plan: "Planned",
  ask_user: "Asked you",
  subagent: "Delegated",
  shell_start: "Started session",
  shell_read: "Read session",
  shell_send: "Sent to session",
  shell_kill: "Killed session",
  shell_list: "Listed sessions",
  list_ssh_hosts: "Listed",
  ssh_download: "Downloaded",
  ssh_upload: "Uploaded",
  docker_ps: "Listed",
  docker_logs: "Read logs",
  docker_exec: "Ran in container",
  docker_inspect: "Inspected",
  share_image: "Shared",
  view_image: "Looked at",
  compact_context: "Compacted",
  memory_search: "Searched past chats",
  memory_read_chat: "Read past chat",
  web_search: "Searched web",
  web_screenshot: "Screenshotted",
  deep_research: "Researched",
  download_file: "Downloaded",
  git_status: "Checked git",
  git_diff: "Diffed",
  git_log: "Read history",
  git_commit: "Committed",
  git_branch: "Branched",
};

function ToolIcon(props: { name: string }) {
  return (
    <Switch fallback={<Code size={13} />}>
      <Match when={props.name === "read_file"}>
        <FileIcon size={13} />
      </Match>
      <Match when={props.name === "write_file" || props.name === "edit_file"}>
        <Diff size={13} />
      </Match>
      <Match when={props.name === "list_dir" || props.name === "glob"}>
        <Folder size={13} />
      </Match>
      <Match when={props.name === "grep"}>
        <Search size={13} />
      </Match>
      <Match when={props.name === "run_command"}>
        <Terminal size={13} />
      </Match>
      <Match when={props.name.startsWith("ssh_") || props.name === "list_ssh_hosts"}>
        <Server size={13} />
      </Match>
      <Match when={props.name.startsWith("docker_")}>
        <Server size={13} />
      </Match>
      <Match
        when={props.name === "share_image" || props.name === "web_screenshot" || props.name === "view_image"}
      >
        <ImageIcon size={13} />
      </Match>
      <Match
        when={
          props.name === "fetch_url" ||
          props.name === "web_search" ||
          props.name === "deep_research" ||
          props.name === "download_file"
        }
      >
        <Globe size={13} />
      </Match>
      <Match when={props.name.startsWith("git_")}>
        <Diff size={13} />
      </Match>
    </Switch>
  );
}

interface Props {
  tool: ToolActivity;
  result?: string | null;
  onApprove?: (callId: string, approved: boolean) => void;
  onAlwaysAllow?: (risk: string) => void;
}

export function ToolRow(props: Props) {
  // A call that needs a decision opens itself: the user cannot judge it without
  // seeing what it will do.
  const [open, setOpen] = createSignal(false);
  const expanded = () => open() || props.tool.status === "awaiting";
  const args = () => parseArgs(props.tool.args);
  const path = () => str(args().path);
  const monospace = () =>
    props.tool.name === "run_command" || props.tool.name === "ssh_exec" || props.tool.name === "docker_exec";

  return (
    <div class="tool" data-status={props.tool.status} data-open={expanded()}>
      <button type="button" class="tool-head" onClick={() => setOpen(!open())} title="Show details">
        <span class="tool-caret">
          <Chevron size={11} />
        </span>
        <span class="tool-icon">
          <ToolIcon name={props.tool.name} />
        </span>
        <span class="tool-verb">{VERBS[props.tool.name] ?? props.tool.name}</span>
        <span class="tool-target" data-mono={monospace()}>
          {describeTool(props.tool.name, props.tool.args)}
        </span>
        <span class="tool-state">
          <Switch>
            <Match when={props.tool.status === "running" || props.tool.status === "pending"}>
              <span class="tool-spinner" />
            </Match>
            <Match when={props.tool.status === "awaiting"}>
              <span class="chip" data-tone="warn">
                needs approval
              </span>
            </Match>
            <Match when={props.tool.status === "declined"}>
              <span class="chip">declined</span>
            </Match>
            <Match when={props.tool.status === "error"}>
              <Warning size={12} />
            </Match>
            <Match when={props.tool.status === "ok"}>
              <Check size={12} />
            </Match>
          </Switch>
        </span>
        <Show when={props.tool.durationMs}>
          {(d) => <span class="tool-time tabular">{formatDuration(d())}</span>}
        </Show>
      </button>

      <Show when={imageOf(props.tool)}>
        {(img) => (
          <figure class="tool-figure">
            <img src={assetUrl(img().path)} alt={img().name} loading="lazy" />
            <Show when={img().caption}>{(c) => <figcaption>{c()}</figcaption>}</Show>
          </figure>
        )}
      </Show>

      <Show when={expanded()}>
        <div class="tool-body selectable">
          <Show when={path()}>
            {(p) => (
              <button type="button" class="tool-open" onClick={() => void openPath(p())}>
                Open {p()}
              </button>
            )}
          </Show>
          <Show when={Object.keys(args()).length > 0}>
            <pre class="tool-args">{JSON.stringify(args(), null, 2)}</pre>
          </Show>

          <Show when={props.tool.status === "awaiting" && props.tool.warning}>
            {(w) => (
              <div class="callout" data-tone="warn">
                <Warning size={14} />
                <span>{w()}</span>
              </div>
            )}
          </Show>
          <Show when={props.tool.status === "awaiting" && props.onApprove}>
            <div class="approval-actions">
              <button
                type="button"
                class="btn"
                data-variant="primary"
                data-size="sm"
                onClick={() => props.onApprove?.(props.tool.id, true)}
              >
                Allow <kbd>⌘</kbd>
                <kbd>↵</kbd>
              </button>
              <button
                type="button"
                class="btn"
                data-variant="outline"
                data-size="sm"
                onClick={() => props.onApprove?.(props.tool.id, false)}
              >
                Decline
              </button>
              <div class="spacer" />
              <Show when={props.onAlwaysAllow && props.tool.risk}>
                {(_) => (
                  <button
                    type="button"
                    class="btn"
                    data-size="sm"
                    title="Raise the permission mode so this kind of tool stops asking"
                    onClick={() => props.onAlwaysAllow?.(props.tool.risk ?? "exec")}
                  >
                    Always allow {props.tool.risk}
                  </button>
                )}
              </Show>
            </div>
          </Show>

          <Show
            when={diffOf(props.tool)}
            fallback={
              <Show when={props.result ?? props.tool.summary}>
                {(r) => <pre class="tool-result">{clip(r())}</pre>}
              </Show>
            }
          >
            {(d) => (
              /* Model-produced diff, coloured by the app's own highlighter,
                 which escapes its input before wrapping tokens. */
              <pre class="tool-result tool-diff" innerHTML={highlight(clip(d()), "diff")} />
            )}
          </Show>
        </div>
      </Show>
    </div>
  );
}

/** The image carried by a share_image display payload. */
function imageOf(tool: ToolActivity): { path: string; name: string; caption: string | null } | null {
  const d = tool.display;
  if (d?.kind !== "image" || typeof d.path !== "string") return null;
  return {
    path: d.path,
    name: typeof d.name === "string" ? d.name : d.path,
    caption: typeof d.caption === "string" ? d.caption : null,
  };
}

/** The unified diff carried by edit_file / write_file display payloads. */
function diffOf(tool: ToolActivity): string | null {
  const d = tool.display?.diff;
  return typeof d === "string" && d.length > 0 ? d : null;
}

function clip(s: string, max = 6000): string {
  return s.length > max ? `${s.slice(0, max)}\n… ${s.length - max} more characters` : s;
}

export { parseArgs };
