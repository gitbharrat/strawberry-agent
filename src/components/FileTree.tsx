// File browser.
//
// Children are fetched only when a directory is expanded, so the cost of
// opening a large repository is the cost of listing one directory.

import { createEffect, createSignal, For, on, onMount, Show } from "solid-js";
import { Chevron, File as FileIcon, Folder, FolderOpen, Plus, Refresh, Trash } from "./Icons";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { formatBytes, openPath, pickDirectory } from "~/lib/actions";
import type { FsEntry } from "~/lib/types";
import { errText } from "~/lib/err";

export function FileTree() {
  // The root follows the chat's workspace live — connecting a remote host or
  // setting a folder re-roots the tree without a remount. A manual browse
  // overrides it until the workspace binding next changes.
  const [override, setOverride] = createSignal<string | null>(null);
  const [home, setHome] = createSignal("");
  const [showHidden, setShowHidden] = createSignal(false);
  const [version, setVersion] = createSignal(0);
  const [creating, setCreating] = createSignal(false);
  const [newName, setNewName] = createSignal("");

  const workspaceRoot = () => store.activeChat()?.cwd ?? store.activeProject()?.rootPath ?? null;
  const root = () => override() ?? workspaceRoot() ?? home();

  onMount(() => {
    void api
      .fsHome()
      .then(setHome)
      .catch(() => {});
  });
  createEffect(on(workspaceRoot, () => setOverride(null), { defer: true }));

  async function chooseRoot() {
    const dir = await pickDirectory("Browse folder");
    if (dir) {
      setOverride(dir);
      setVersion((v) => v + 1);
    }
  }

  /** Creates an empty file under the root (nested paths make their folders),
   * journals it against the active chat, and opens it in the editor. */
  async function createFile() {
    const name = newName().trim().replace(/^\/+/, "");
    setCreating(false);
    setNewName("");
    if (!name) return;
    // A stray ".." would silently write outside the folder being browsed.
    if (name.split("/").some((part) => part === "..")) {
      toast("warn", "Paths cannot step outside this folder");
      return;
    }
    const path = `${root().replace(/\/+$/, "")}/${name}`;
    const exists = await api.fsRead(path).then(
      () => true,
      () => false,
    );
    if (exists) {
      toast("warn", `${name} already exists`);
      return;
    }
    try {
      await api.fsWrite({ path, content: "", chatId: store.activeChatId() ?? undefined });
      setVersion((v) => v + 1);
      await openPath(path);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  return (
    <div class="tree">
      <div class="tree-head">
        <button type="button" class="tree-root truncate" onClick={() => void chooseRoot()} title={root()}>
          <Folder size={13} />
          <span class="truncate">{root().split("/").filter(Boolean).pop() ?? root()}</span>
        </button>
        <div class="spacer" />
        <label class="tree-toggle" title="Show hidden files">
          <input
            type="checkbox"
            checked={showHidden()}
            onChange={(e) => {
              setShowHidden(e.currentTarget.checked);
              setVersion((v) => v + 1);
            }}
          />
          <span>hidden</span>
        </label>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="New file (folders in the path are created)"
          onClick={() => setCreating(true)}
        >
          <Plus size={12} />
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Refresh"
          onClick={() => setVersion((v) => v + 1)}
        >
          <Refresh size={12} />
        </button>
      </div>
      <Show when={creating()}>
        <input
          class="input tree-new"
          data-mono="true"
          autofocus
          placeholder="name.ts or sub/dir/name.ts"
          value={newName()}
          onInput={(e) => setNewName(e.currentTarget.value)}
          onBlur={() => void createFile()}
          onKeyDown={(e) => {
            if (e.key === "Enter") void createFile();
            if (e.key === "Escape") {
              setCreating(false);
              setNewName("");
            }
          }}
        />
      </Show>
      <div class="tree-scroll">
        <Show
          when={root()}
          fallback={
            <div class="empty">
              <p>Choose a folder to browse.</p>
            </div>
          }
        >
          <Directory
            path={root()}
            depth={0}
            showHidden={showHidden()}
            version={version()}
            onChanged={() => setVersion((v) => v + 1)}
          />
        </Show>
      </div>
    </div>
  );
}

function Directory(props: {
  path: string;
  depth: number;
  showHidden: boolean;
  version: number;
  onChanged: () => void;
}) {
  const [entries, setEntries] = createSignal<FsEntry[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  const load = async () => {
    try {
      setEntries(await api.fsList(props.path, props.showHidden));
      setError(null);
    } catch (e) {
      setError(errText(e));
    }
  };

  onMount(load);
  // `path` is in the deps: the root directory component is reused in place
  // when the tree re-roots, so a path change must trigger its own reload.
  createEffect(on(() => [props.path, props.version, props.showHidden], load, { defer: true }));

  return (
    <div>
      <Show when={error()}>{(e) => <div class="tree-error muted">{e()}</div>}</Show>
      <For each={entries() ?? []}>
        {(entry) => (
          <Entry
            entry={entry}
            depth={props.depth}
            showHidden={props.showHidden}
            version={props.version}
            onChanged={props.onChanged}
          />
        )}
      </For>
      <Show when={entries()?.length === 0}>
        <div class="tree-empty muted" style={{ "padding-left": `${props.depth * 14 + 24}px` }}>
          empty
        </div>
      </Show>
    </div>
  );
}

function Entry(props: {
  entry: FsEntry;
  depth: number;
  showHidden: boolean;
  version: number;
  onChanged: () => void;
}) {
  const [open, setOpen] = createSignal(false);
  const active = () => {
    const t = store.activeTab()?.surface;
    return t?.kind === "editor" && t.path === props.entry.path;
  };

  const activate = () => {
    if (props.entry.isDir) setOpen(!open());
    else void openPath(props.entry.path).catch((e) => toast("error", errText(e)));
  };

  /** Files are journalled and undoable; folders only go when empty, with an
   * explicit second step for a recursive delete (that one has no undo). */
  async function remove() {
    const e = props.entry;
    if (e.isDir) {
      try {
        await api.fsDelete(e.path, undefined, false);
        toast("ok", `Deleted ${e.name}/`);
        props.onChanged();
      } catch (err) {
        if (String(err).includes("not empty")) {
          toast("warn", `${e.name}/ is not empty`, {
            label: "Delete everything",
            run: () => {
              void api
                .fsDelete(e.path, undefined, true)
                .then(() => {
                  toast("ok", `Deleted ${e.name}/ and its contents`);
                  props.onChanged();
                })
                .catch((e2) => toast("error", String(e2)));
            },
          });
        } else {
          toast("error", String(err));
        }
      }
      return;
    }
    try {
      const changeId = await api.fsDelete(e.path, store.activeChatId() ?? undefined, false);
      props.onChanged();
      toast(
        "ok",
        `Deleted ${e.name}`,
        changeId
          ? {
              label: "Undo",
              run: () => {
                void api
                  .revertChange(changeId)
                  .then(() => props.onChanged())
                  .catch((e2) => toast("error", String(e2)));
              },
            }
          : undefined,
      );
    } catch (err) {
      toast("error", String(err));
    }
  }

  return (
    <>
      <div class="tree-row" data-dir={props.entry.isDir} data-active={active()}>
        <button
          type="button"
          class="tree-row-main"
          style={{ "padding-left": `${props.depth * 14 + 8}px` }}
          onClick={activate}
          title={props.entry.path}
        >
          <span class="tree-caret" data-open={open()}>
            <Show when={props.entry.isDir}>
              <Chevron size={10} />
            </Show>
          </span>
          <Show when={props.entry.isDir} fallback={<FileIcon size={13} />}>
            <Show when={open()} fallback={<Folder size={13} />}>
              <FolderOpen size={13} />
            </Show>
          </Show>
          <span class="truncate">{props.entry.name}</span>
          <Show when={!props.entry.isDir && props.entry.size > 0}>
            <span class="tree-size muted tabular">{formatBytes(props.entry.size)}</span>
          </Show>
        </button>
        <span class="tree-actions">
          <button
            type="button"
            class="btn"
            data-size="icon-sm"
            title={props.entry.isDir ? "Delete folder" : "Delete file (undoable)"}
            onClick={() => void remove()}
          >
            <Trash size={11} />
          </button>
        </span>
      </div>
      <Show when={open()}>
        <Directory
          path={props.entry.path}
          depth={props.depth + 1}
          showHidden={props.showHidden}
          version={props.version}
          onChanged={props.onChanged}
        />
      </Show>
    </>
  );
}
