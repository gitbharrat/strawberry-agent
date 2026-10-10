import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { Resvg } from "@resvg/resvg-js";

const svg = readFileSync("src/assets/logo.svg", "utf8");

// Icon canvas: the mark inset on a transparent square, matching platform
// padding conventions (~10% breathing room).
const SIZE = 1024;
const INSET = 0.09;
const inner = Math.round(SIZE * (1 - INSET * 2));
const off = Math.round(SIZE * INSET);
const wrapped = `<svg xmlns="http://www.w3.org/2000/svg" width="${SIZE}" height="${SIZE}" viewBox="0 0 ${SIZE} ${SIZE}">
  <g transform="translate(${off} ${off}) scale(${inner / 512})">${svg.replace(/<\/?svg[^>]*>/g, "")}</g>
</svg>`;

mkdirSync("src-tauri/icons", { recursive: true });
const png = new Resvg(wrapped, { fitTo: { mode: "width", value: SIZE } }).render().asPng();
writeFileSync("src-tauri/icons/source.png", png);

// A flat preview for eyeballing the mark during development.
const preview = new Resvg(svg, { fitTo: { mode: "width", value: 512 }, background: "#141013" })
  .render()
  .asPng();
writeFileSync("tools/logo-preview.png", preview);
console.log("wrote src-tauri/icons/source.png and tools/logo-preview.png");
