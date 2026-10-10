// The side pane: a tab strip over a registry of surfaces.
//
// Surfaces are keyed by `kind`, so the planned node-graph and design-canvas
// panes drop in by adding a case here and a variant to `PaneSurface` — nothing
// else in the app needs to know they exist.
//
// Inactive tabs stay mounted but hidden. That matters for the terminal, whose
// PTY must not be torn down just because the user looked at a diff, and it
// makes switching tabs free.

import { For, lazy, Match, Show, Suspense, Switch } from "solid-js";
import { BrowserPane } from "./BrowserPane";
import { ActivityPane } from "./ActivityPane";
import { PlanPane } from "./PlanPane";
import { ChangesPanel } from "./ChangesPanel";
import { CodeView } from "./CodeView";
import { DiffView } from "./DiffView";
import { FileTree } from "./FileTree";
import { HtmlPreview, ImageViewer, SvgViewer } from "./Viewers";
import {
  Check,
  Close,
  Diff,
  Edit,
  File as FileIcon,
  Globe,
  Image as ImageIcon,
  Layers,
  Maximize,
  Minimize,
  Network,
  NodeGraph,
  Play,
  Terminal,
} from "./Icons";
import { store, type PaneTab } from "~/lib/store";

// xterm is the one genuinely heavy surface, so it is code-split; the graph
// pane rides along for free bundle-wise until someone opens it.
const TerminalPane = lazy(() => import("./TerminalPane").then((m) => ({ default: m.TerminalPane })));
const AnalysisPane = lazy(() => import("./AnalysisPane").then((m) => ({ default: m.AnalysisPane })));
const GraphPane = lazy(() => import("./GraphPane").then((m) => ({ default: m.GraphPane })));
const SketchPad = lazy(() => import("./SketchPad").then((m) => ({ default: m.SketchPad })));

function TabIcon(props: { tab: PaneTab }) {
  return (
    <Switch fallback={<FileIcon size={12} />}>
      <Match when={props.tab.surface.kind === "terminal"}>
        <Terminal size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "activity"}>
        <Play size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "plan"}>
        <Check size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "files"}>
        <Layers size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "changes"}>
        <Diff size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "analysis"}>
        <Network size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "graph"}>
        <NodeGraph size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "sketch"}>
        <Edit size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "diff"}>
        <Diff size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "browser"}>
        <Globe size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "preview"}>
        <Globe size={12} />
      </Match>
      <Match when={props.tab.surface.kind === "image" || props.tab.surface.kind === "svg"}>
        <ImageIcon size={12} />
      </Match>
    </Switch>
  );
}

function Surface(props: { tab: PaneTab }) {
  const s = () => props.tab.surface;
  return (
    <Suspense
      fallback={
        <div class="empty">
          <p>Loading…</p>
        </div>
      }
    >
      <Switch>
        <Match when={s().kind === "terminal"}>
          <TerminalPane />
        </Match>
        <Match when={s().kind === "activity"}>
          <ActivityPane />
        </Match>
        <Match when={s().kind === "plan"}>
          <PlanPane tabId={props.tab.id} />
        </Match>
        <Match when={s().kind === "files"}>
          <FileTree />
        </Match>
        <Match when={s().kind === "changes"}>
          <ChangesPanel />
        </Match>
        <Match when={s().kind === "analysis"}>
          <AnalysisPane />
        </Match>
        <Match when={s().kind === "graph"}>
          <GraphPane />
        </Match>
        <Match when={s().kind === "sketch"}>
          <SketchPad />
        </Match>
        <Match when={s()} keyed>
          {(surface) => (
            <Switch>
              <Match when={surface.kind === "browser" && surface}>
                {(b) => <BrowserPane tabId={props.tab.id} url={b().url} />}
              </Match>
              <Match when={surface.kind === "editor" && surface}>
                {(e) => <CodeView path={e().path} line={e().line} tabId={props.tab.id} />}
              </Match>
              <Match when={surface.kind === "diff" && surface}>
                {(d) => <DiffView changeId={d().changeId} tabId={props.tab.id} />}
              </Match>
              <Match when={surface.kind === "image" && surface}>
                {(i) => <ImageViewer path={i().path} tabId={props.tab.id} />}
              </Match>
              <Match when={surface.kind === "svg" && surface}>
                {(i) => <SvgViewer path={i().path} tabId={props.tab.id} />}
              </Match>
              <Match when={surface.kind === "preview" && surface}>
                {(p) => <HtmlPreview html={p().html} title={p().title} tabId={props.tab.id} />}
              </Match>
            </Switch>
          )}
        </Match>
      </Switch>
    </Suspense>
  );
}

export function Pane() {
  // Closed tabs stay in paneTabs (mounted, hidden) — only visible ones make
  // the strip. With none visible the aside display:none's itself rather than
  // unmounting, so terminals and canvases survive a full pane close.
  return (
    <aside class="pane" data-hidden={!store.paneOpen()}>
      <div class="pane-tabs">
        <For each={store.visiblePaneTabs()}>
          {(tab) => (
            <div class="pane-tab" data-active={tab.id === store.activeTabId()}>
              <button
                type="button"
                class="pane-tab-main"
                onClick={() => store.setActiveTabId(tab.id)}
                onAuxClick={(e) => {
                  if (e.button === 1) store.closeTab(tab.id);
                }}
                title={tab.title}
              >
                <TabIcon tab={tab} />
                <span class="truncate">{tab.title}</span>
              </button>
              <button
                type="button"
                class="pane-tab-close"
                aria-label={`Close ${tab.title}`}
                onClick={() => store.closeTab(tab.id)}
              >
                <Close size={10} />
              </button>
            </div>
          )}
        </For>
        <div class="spacer" />
        <span class="vdivider" />
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          data-on={store.paneMaximized()}
          title={store.paneMaximized() ? "Restore split  ⌘⇧M" : "Maximize pane  ⌘⇧M"}
          onClick={() => store.setPaneMaximized(!store.paneMaximized())}
        >
          <Show when={store.paneMaximized()} fallback={<Maximize size={12} />}>
            <Minimize size={12} />
          </Show>
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close pane  ⌘J"
          onClick={() => store.closePane()}
        >
          <Close size={12} />
        </button>
      </div>

      <div class="pane-stack">
        <For each={store.paneTabs()}>
          {(tab) => (
            <div class="pane-surface" data-active={tab.id === store.activeTabId()}>
              <Surface tab={tab} />
            </div>
          )}
        </For>
        <Show when={store.visiblePaneTabs().length === 0}>
          <div class="empty">
            <p>Nothing open.</p>
          </div>
        </Show>
      </div>
    </aside>
  );
}
