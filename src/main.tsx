import { render } from "solid-js/web";
import { App } from "./App";
import "./styles/theme.css";
import "./styles/app.css";
import "./styles/chat.css";
import "./styles/panes.css";
import "./styles/analysis.css";
import "./styles/graph.css";

// macOS floats its window controls over the content, so the layout needs to
// know which platform it is on before the first paint. Sniffing the user agent
// is synchronous; the OS plugin is not, and a reflow here would be visible.
const ua = navigator.userAgent;
document.documentElement.dataset.os = ua.includes("Mac OS X")
  ? "macos"
  : ua.includes("Windows")
    ? "windows"
    : "linux";

const root = document.getElementById("root");
if (!root) throw new Error("#root is missing from index.html");

render(() => <App />, root);

// The window is created hidden; reveal it once the first frame is on screen so
// there is no flash of unstyled or mis-themed content.
requestAnimationFrame(() => {
  void import("@tauri-apps/api/window")
    .then(({ getCurrentWindow }) => getCurrentWindow().show())
    .catch(() => {});
});
