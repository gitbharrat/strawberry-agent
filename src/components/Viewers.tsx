// Image, SVG and HTML surfaces.
//
// Images and SVGs come through Tauri's asset protocol so nothing is copied or
// base64-inflated. HTML is rendered inside a sandboxed iframe with no
// same-origin access — a preview must never be able to reach the app.

import { convertFileSrc } from "@tauri-apps/api/core";
import { createResource, createSignal, Show } from "solid-js";
import { Close, Globe, Image as ImageIcon, Refresh } from "./Icons";
import { api } from "~/lib/ipc";
import { store } from "~/lib/store";
import { formatBytes } from "~/lib/actions";

export function assetUrl(path: string): string {
  // The asset protocol's URL shape is per-PLATFORM, not per-build: Windows and
  // Linux serve custom protocols over http://asset.localhost, macOS registers
  // the asset:// scheme — in dev and release alike. Keying this on DEV broke
  // every image in a Mac dev build. convertFileSrc is the reference
  // implementation and always matches the running webview.
  return convertFileSrc(path);
}

export function ImageViewer(props: { path: string; tabId: string }) {
  const [zoom, setZoom] = createSignal(1);
  const [fit, setFit] = createSignal(true);
  const [broken, setBroken] = createSignal(false);
  const name = () => props.path.split("/").pop() ?? props.path;

  return (
    <div class="viewer">
      <div class="code-head">
        <ImageIcon size={13} />
        <span class="truncate" title={props.path}>
          {name()}
        </span>
        <div class="spacer" />
        <button
          type="button"
          class="btn"
          data-size="sm"
          onClick={() => {
            setFit(!fit());
            setZoom(1);
          }}
        >
          {fit() ? "Actual size" : "Fit"}
        </button>
        <Show when={!fit()}>
          <button
            type="button"
            class="btn"
            data-size="icon-sm"
            onClick={() => setZoom((z) => Math.max(0.1, z / 1.4))}
          >
            −
          </button>
          <span class="muted tabular">{Math.round(zoom() * 100)}%</span>
          <button
            type="button"
            class="btn"
            data-size="icon-sm"
            onClick={() => setZoom((z) => Math.min(8, z * 1.4))}
          >
            +
          </button>
        </Show>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close"
          onClick={() => store.closeTab(props.tabId)}
        >
          <Close size={12} />
        </button>
      </div>
      <div class="viewer-body" data-checker="true">
        <Show
          when={!broken()}
          fallback={
            <div class="empty">
              <p>Could not load {name()} — the file may have moved or is not a readable image.</p>
            </div>
          }
        >
          <img
            src={assetUrl(props.path)}
            alt={name()}
            onError={() => setBroken(true)}
            style={
              fit()
                ? { "max-width": "100%", "max-height": "100%" }
                : { transform: `scale(${zoom()})`, "transform-origin": "top left" }
            }
          />
        </Show>
      </div>
    </div>
  );
}

export function SvgViewer(props: { path: string; tabId: string }) {
  const [source, setSource] = createSignal(false);
  const [file] = createResource(
    () => props.path,
    (p) => api.fsRead(p),
  );
  const name = () => props.path.split("/").pop() ?? props.path;

  return (
    <div class="viewer">
      <div class="code-head">
        <ImageIcon size={13} />
        <span class="truncate" title={props.path}>
          {name()}
        </span>
        <Show when={file()}>{(f) => <span class="muted tabular">{formatBytes(f().size)}</span>}</Show>
        <div class="spacer" />
        <button type="button" class="btn" data-size="sm" onClick={() => setSource(!source())}>
          {source() ? "Preview" : "Source"}
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close"
          onClick={() => store.closeTab(props.tabId)}
        >
          <Close size={12} />
        </button>
      </div>
      <Show
        when={source()}
        fallback={
          <div class="viewer-body" data-checker="true">
            <img
              src={assetUrl(props.path)}
              alt={name()}
              style={{ "max-width": "100%", "max-height": "100%" }}
            />
          </div>
        }
      >
        <pre class="viewer-source selectable">{file()?.content ?? ""}</pre>
      </Show>
    </div>
  );
}

export function HtmlPreview(props: { html: string; title: string; tabId: string }) {
  const [nonce, setNonce] = createSignal(0);
  const [source, setSource] = createSignal(false);

  return (
    <div class="viewer">
      <div class="code-head">
        <Globe size={13} />
        <span class="truncate">{props.title}</span>
        <span class="chip" title="Scripts run in an isolated origin with no access to this app">
          sandboxed
        </span>
        <div class="spacer" />
        <button type="button" class="btn" data-size="sm" onClick={() => setSource(!source())}>
          {source() ? "Preview" : "Source"}
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Reload"
          onClick={() => setNonce((n) => n + 1)}
        >
          <Refresh size={12} />
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close"
          onClick={() => store.closeTab(props.tabId)}
        >
          <Close size={12} />
        </button>
      </div>
      <Show
        when={source()}
        fallback={
          <iframe
            class="viewer-frame"
            title={props.title}
            // No allow-same-origin: the frame gets a unique opaque origin, so it
            // cannot touch this app's storage or DOM even though scripts run.
            sandbox="allow-scripts allow-forms allow-popups"
            srcdoc={`${props.html}<!--${nonce()}-->`}
          />
        }
      >
        <pre class="viewer-source selectable">{props.html}</pre>
      </Show>
    </div>
  );
}
