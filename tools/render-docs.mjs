// Run with Node 23+ (TypeScript stripping) after npm install.
import { writeFileSync, mkdirSync } from "node:fs";
import { Resvg } from "@resvg/resvg-js";
import { NODE_TYPES, CATEGORY_COLOR } from "../src/lib/meg.ts";
const out = new URL("../docs/images/", import.meta.url);
mkdirSync(out, { recursive: true });
const esc = (s) => String(s).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
const text = (x, y, s, size = 18, color = "#e9e7ee") =>
  `<text x="${x}" y="${y}" fill="${color}" font-family="Arial, sans-serif" font-size="${size}">${esc(s)}</text>`;
const box = (x, y, w, h, c = "#292832") =>
  `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="12" fill="#191820" stroke="${c}"/>`;
function save(name, w, h, body) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="#a49ab8"/></marker></defs><rect width="100%" height="100%" fill="#100f16"/>${body}</svg>`;
  writeFileSync(new URL(name + ".svg", out), svg);
  writeFileSync(
    new URL(name + ".png", out),
    new Resvg(svg, { font: { loadSystemFonts: true } }).render().asPng(),
  );
}
const title = (name, sub) => text(48, 62, name, 32) + text(48, 96, sub, 16, "#a8a2b5");
const card = (x, y, w, name, sub, color) =>
  box(x, y, w, 100, color) + text(x + 18, y + 34, name, 21, color) + text(x + 18, y + 68, sub, 14);
const line = (x1, y1, x2, y2) =>
  `<path d="M${x1} ${y1} L${x2} ${y2}" stroke="#a49ab8" stroke-width="2" fill="none" marker-end="url(#arrow)"/>`;
let body = title(
  "Strawberry Agent / the execution loop",
  "WORK IN PROGRESS · Essential template · source-backed illustration",
);
const xs = [48, 338, 628, 918],
  names = ["Goal", "Execute", "Verify", "Output"],
  subs = ["Your request", "Agent + workspace tools", "Check against the goal", "Compose the answer"],
  colors = ["#8c8c8c", "#d99a34", "#3fd082", "#a49ab8"];
for (let i = 0; i < 4; i++) {
  body += card(xs[i], 170, 230, names[i], subs[i], colors[i]);
  if (i < 3) body += line(xs[i] + 230, 220, xs[i + 1] - 8, 220);
}
body += text(872, 200, "pass", 13, "#3fd082");
body +=
  '<path d="M743 270 L743 340 L453 340 L453 280" fill="none" stroke="#a49ab8" stroke-width="2" stroke-dasharray="7 5" marker-end="url(#arrow)"/>';
body += text(490, 329, "fail → fix · loop budget: 2", 16, "#ff8f9f");
body += text(
  48,
  412,
  "Per-node model + reasoning effort     •     Cached outputs + pinning     •     Bounded feedback loops",
  17,
);
save("execution-loop", 1200, 465, body);
body = title(
  "25 node types / one composable harness",
  "WORK IN PROGRESS · Names and category colours read directly from src/lib/meg.ts",
);
NODE_TYPES.forEach((n, i) => {
  const x = 48 + (i % 5) * 230,
    y = 140 + Math.floor(i / 5) * 115;
  body += card(x, y, 214, n.title, n.category.toUpperCase(), CATEGORY_COLOR[n.category]);
});
save("node-catalog", 1240, 765, body);
body = title(
  "Build once / reuse a custom node",
  "WORK IN PROGRESS · Local reusable groups · illustrative flow, not an application screenshot",
);
body +=
  card(48, 155, 250, "Research", "Gather useful context", "#6892c0") +
  card(48, 280, 250, "Plan", "Choose the next steps", "#9585d6") +
  card(48, 405, 250, "Verify", "Check the outcome", "#3fd082");
body +=
  line(298, 330, 425, 330) +
  card(445, 275, 305, "Save as custom node…", "Selection + internal connections", "#ff8f9f") +
  line(750, 330, 865, 330) +
  card(885, 275, 290, "Reusable group", "Add menu → another graph", "#ff8f9f");
body +=
  text(445, 420, "Fresh node IDs when inserted", 18) +
  text(445, 452, "Model choices and prompts are copied", 18) +
  text(445, 484, "Notes and external connections are excluded", 18);
body += text(
  48,
  585,
  "Custom nodes are stored locally. A dedicated cross-device sharing / import-export flow is not implemented.",
  17,
  "#a8a2b5",
);
save("reusable-nodes", 1240, 635, body);
console.log("Rendered 3 SVG diagrams and 3 PNG previews.");
