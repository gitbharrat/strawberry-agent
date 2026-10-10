// A compact CommonMark-ish renderer built for streaming.
//
// Two things matter here that a general-purpose library does not give us:
//
//  1. **Incremental rendering.** `splitStable` finds the last point in a
//     partially received document that can no longer change, so a streaming
//     message re-renders only its final block instead of the whole transcript
//     on every frame.
//  2. **Size.** markdown-it plus a highlighter is larger than the rest of this
//     application put together, for a feature set chat output never uses.
//
// Everything is escaped on the way out; model output is untrusted input.

import { escapeHtml, highlight, languageLabel } from "./highlight";

export interface RenderOptions {
  /** Emit placeholders that the view upgrades into mermaid diagrams. */
  mermaid?: boolean;
  /** Emit placeholders that the view upgrades into rendered maths. */
  math?: boolean;
}

const FENCE = /^(\s*)(`{3,}|~{3,})\s*([^\s`]*)\s*$/;
const HEADING = /^(#{1,6})\s+(.*)$/;
const HR = /^\s*([-*_])(?:\s*\1){2,}\s*$/;
const UL = /^(\s*)([-*+])\s+(.*)$/;
const OL = /^(\s*)(\d{1,9})[.)]\s+(.*)$/;
const QUOTE = /^\s*>\s?(.*)$/;
const TABLE_SEP = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)+\|?\s*$/;

/**
 * Splits a partially streamed document into a prefix that is final and a tail
 * that may still change.
 *
 * The boundary is the last blank line that is not inside a fenced code block.
 * Everything before it is structurally settled: no later token can alter how it
 * renders.
 */
export function splitStable(src: string): { stable: string; tail: string } {
  const lines = src.split("\n");
  let fence: string | null = null;
  let boundary = -1;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const m = line.match(FENCE);
    if (m) {
      const marker = m[2]!;
      if (fence === null) fence = marker[0]!.repeat(3);
      else if (marker.startsWith(fence)) fence = null;
      continue;
    }
    if (fence === null && line.trim() === "") boundary = i;
  }

  if (boundary <= 0) return { stable: "", tail: src };
  return {
    stable: lines.slice(0, boundary).join("\n"),
    tail: lines.slice(boundary).join("\n"),
  };
}

// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: a block dispatcher; splitting it would hide the grammar
export function renderMarkdown(src: string, opts: RenderOptions = {}): string {
  const lines = src.split("\n");
  const out: string[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i]!;

    if (line.trim() === "") {
      i++;
      continue;
    }

    // ---- fenced code
    const fence = line.match(FENCE);
    if (fence) {
      const marker = fence[2]!;
      const lang = (fence[3] ?? "").toLowerCase();
      const body: string[] = [];
      i++;
      while (i < lines.length) {
        const m = lines[i]!.match(FENCE);
        if (m && m[2]!.startsWith(marker[0]!.repeat(3)) && m[2]!.length >= marker.length) {
          i++;
          break;
        }
        body.push(lines[i]!);
        i++;
      }
      out.push(renderCode(body.join("\n"), lang, opts));
      continue;
    }

    // ---- heading
    const h = line.match(HEADING);
    if (h) {
      const level = h[1]!.length;
      out.push(`<h${level}>${inline(h[2]!, opts)}</h${level}>`);
      i++;
      continue;
    }

    // ---- horizontal rule
    if (HR.test(line)) {
      out.push("<hr>");
      i++;
      continue;
    }

    // ---- blockquote
    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (i < lines.length && (QUOTE.test(lines[i]!) || (body.length > 0 && lines[i]!.trim() !== ""))) {
        const m = lines[i]!.match(QUOTE);
        body.push(m ? m[1]! : lines[i]!);
        i++;
      }
      out.push(`<blockquote>${renderMarkdown(body.join("\n"), opts)}</blockquote>`);
      continue;
    }

    // ---- table
    if (line.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]!)) {
      const header = splitRow(line);
      const aligns = splitRow(lines[i + 1]!).map(alignOf);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length && lines[i]!.includes("|") && lines[i]!.trim() !== "") {
        rows.push(splitRow(lines[i]!));
        i++;
      }
      out.push(renderTable(header, aligns, rows, opts));
      continue;
    }

    // ---- lists
    if (UL.test(line) || OL.test(line)) {
      const [html, next] = renderList(lines, i, opts);
      out.push(html);
      i = next;
      continue;
    }

    // ---- paragraph
    const para: string[] = [];
    while (
      i < lines.length &&
      lines[i]!.trim() !== "" &&
      !FENCE.test(lines[i]!) &&
      !HEADING.test(lines[i]!) &&
      !HR.test(lines[i]!) &&
      !QUOTE.test(lines[i]!) &&
      !UL.test(lines[i]!) &&
      !OL.test(lines[i]!)
    ) {
      para.push(lines[i]!);
      i++;
    }
    if (para.length) out.push(`<p>${inline(para.join("\n"), opts)}</p>`);
  }

  return out.join("");
}

interface DesignSpecJson {
  theme?: { name?: string; colors?: { role?: string; hex?: string }[] };
  wireframe?: {
    name?: string;
    cols?: number;
    rows?: number;
    areas?: { label?: string; x?: number; y?: number; w?: number; h?: number; role?: string }[];
  };
}

const hexOk = (h: unknown): h is string => typeof h === "string" && /^#[0-9a-f]{3,8}$/i.test(h);

/** A design node's fenced JSON — theme swatches and a 12×8 wireframe —
 * rendered as the real thing instead of a wall of JSON. Anything that does
 * not match the design schema falls through to the normal code block. */
function tryDesignHtml(code: string): string | null {
  let spec: DesignSpecJson;
  try {
    spec = JSON.parse(code);
  } catch {
    return null;
  }
  const colors = (spec?.theme?.colors ?? []).filter((c) => hexOk(c?.hex));
  const wf = spec?.wireframe;
  const areas = Array.isArray(wf?.areas) ? wf.areas : [];
  if (colors.length === 0 && areas.length === 0) return null;

  // The raw JSON rides along so the view can upgrade this block into a live
  // editor (click a swatch to recolour, drag an area, sketch over it).
  let html = `<div class="md-design" data-design="${escapeHtml(code)}">`;
  const title = spec.theme?.name ?? wf?.name ?? "Design";
  html += `<div class="md-design-head"><span>${escapeHtml(String(title))}</span><span class="md-design-tag">design</span></div>`;
  if (colors.length > 0) html += swatchesHtml(spec.theme?.colors ?? []);
  if (areas.length > 0) html += wireframeHtml(wf, areas);
  return `${html}</div>`;
}

function swatchesHtml(all: { role?: string; hex?: string }[]): string {
  const luma = (hex: string): number => {
    const h = hex.length < 6 ? hex.replace(/[0-9a-f]/gi, (c) => c + c) : hex;
    const n = Number.parseInt(h.slice(1, 7), 16);
    return (0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255)) / 255;
  };
  let html = `<div class="md-design-swatches">`;
  // Index into the ORIGINAL colors array, so edits land on the right entry
  // even when some entries had unusable hex values.
  for (let i = 0; i < all.length; i++) {
    const c = all[i];
    if (!c || !hexOk(c.hex)) continue;
    const hex = c.hex;
    const fg = luma(hex) > 0.55 ? "#111" : "#f5f5f5";
    html +=
      `<div class="md-design-swatch" data-i="${i}" data-hex="${escapeHtml(hex)}" style="background:${escapeHtml(hex)};color:${fg}" title="${escapeHtml(hex)}">` +
      `<b>${escapeHtml(c.role ?? "")}</b><code>${escapeHtml(hex)}</code></div>`;
  }
  return `${html}</div>`;
}

function wireframeHtml(
  wf: DesignSpecJson["wireframe"],
  areas: NonNullable<NonNullable<DesignSpecJson["wireframe"]>["areas"]>,
): string {
  const cols = Math.max(1, Math.min(24, Math.round(wf?.cols ?? 12)));
  const rows = Math.max(1, Math.min(24, Math.round(wf?.rows ?? 8)));
  const clamp = (v: unknown, max: number) =>
    Math.max(0, Math.min(max, Math.round(typeof v === "number" ? v : 0)));
  let html = `<div class="md-design-wireframe" data-cols="${cols}" data-rows="${rows}" style="grid-template-columns:repeat(${cols},1fr);grid-template-rows:repeat(${rows},1fr);aspect-ratio:${cols} / ${rows}">`;
  for (let i = 0; i < Math.min(areas.length, 40); i++) {
    const a = areas[i]!;
    const x = clamp(a.x, cols - 1);
    const y = clamp(a.y, rows - 1);
    const w = Math.max(1, clamp(a.w ?? 1, cols - x));
    const h = Math.max(1, clamp(a.h ?? 1, rows - y));
    const role = typeof a.role === "string" ? a.role : "content";
    html +=
      `<div class="md-design-area" data-i="${i}" data-role="${escapeHtml(role)}" ` +
      `data-x="${x}" data-y="${y}" data-w="${w}" data-h="${h}" ` +
      `style="grid-column:${x + 1} / span ${w};grid-row:${y + 1} / span ${h}">` +
      `<span class="md-design-area-label">${escapeHtml(a.label ?? role)}</span></div>`;
  }
  return `${html}</div>`;
}

/** A complete fenced SVG rendered as the actual graphic. The source rides in
 * a hidden <code> so the copy button still copies markup, and the render goes
 * through an <img> data URL — an image context, where scripts, event handlers
 * and external loads are inert by construction. A block still streaming (no
 * closing tag yet) falls through to the normal code view until it settles. */
function trySvgHtml(code: string): string | null {
  const body = code.trim();
  if (body.length === 0 || body.length > 200_000) return null;
  if (!/^(?:<\?xml[\s\S]{0,200}?\?>\s*)?(?:<!DOCTYPE[^>]*>\s*)?<svg[\s>]/i.test(body)) return null;
  if (!/<\/svg>\s*$/i.test(body)) return null;
  const src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(body)}`;
  return (
    `<figure class="md-code md-svgfig" data-lang="svg">` +
    `<figcaption><span>SVG · rendered</span><button class="md-copy" type="button" data-copy>Copy</button></figcaption>` +
    `<div class="md-svg-view"><img src="${escapeHtml(src)}" alt="SVG graphic" loading="lazy"></div>` +
    `<pre class="md-svg-src"><code>${escapeHtml(body)}</code></pre>` +
    `</figure>`
  );
}

/** A verify/test verdict — `{"pass": bool, "score", "issues", "summary"}` —
 * rendered as a pass/fail card instead of a wall of JSON. */
function tryVerdictHtml(code: string): string | null {
  let v: { pass?: unknown; score?: unknown; issues?: unknown; summary?: unknown };
  try {
    v = JSON.parse(code);
  } catch {
    return null;
  }
  if (typeof v?.pass !== "boolean") return null;
  const hasShape = Array.isArray(v.issues) || typeof v.summary === "string" || typeof v.score === "number";
  if (!hasShape) return null;

  const pass = v.pass;
  const score = typeof v.score === "number" ? Math.max(0, Math.min(1, v.score)) : null;
  const issues = (Array.isArray(v.issues) ? v.issues : [])
    .map((i) => (typeof i === "string" ? i : JSON.stringify(i)))
    .slice(0, 12);
  const summary = typeof v.summary === "string" ? v.summary : "";

  let html = `<div class="md-verdict" data-pass="${pass}">`;
  html += `<div class="md-verdict-head">`;
  html += `<span class="md-verdict-badge">${pass ? "✓ passed" : "✗ failed"}</span>`;
  if (score !== null) {
    html +=
      `<span class="md-verdict-score tabular">${Math.round(score * 100)}%</span>` +
      `<span class="md-verdict-meter"><i style="width:${Math.round(score * 100)}%"></i></span>`;
  }
  html += `<span class="md-design-tag">verification</span></div>`;
  if (summary) html += `<div class="md-verdict-summary">${escapeHtml(summary)}</div>`;
  if (issues.length > 0) {
    html += `<ul class="md-verdict-issues">`;
    for (const i of issues) html += `<li>${escapeHtml(i)}</li>`;
    html += `</ul>`;
  } else if (pass) {
    html += `<div class="md-verdict-summary md-verdict-clear">No issues found.</div>`;
  }
  return `${html}</div>`;
}

function renderCode(code: string, lang: string, opts: RenderOptions): string {
  if (lang === "svg" || lang === "xml" || lang === "html" || lang === "") {
    const svg = trySvgHtml(code);
    if (svg) return svg;
  }
  if (lang === "json" || lang === "design" || lang === "") {
    const verdict = tryVerdictHtml(code);
    if (verdict) return verdict;
    const design = tryDesignHtml(code);
    if (design) return design;
  }
  if (opts.mermaid !== false && (lang === "mermaid" || lang === "flowchart")) {
    // Upgraded to an SVG diagram by the view once mermaid has loaded.
    return `<div class="md-mermaid" data-src="${escapeHtml(code)}"><pre class="md-mermaid-fallback">${escapeHtml(code)}</pre></div>`;
  }
  if (opts.math !== false && (lang === "math" || lang === "latex" || lang === "tex")) {
    return `<div class="md-math md-math-block" data-tex="${escapeHtml(code)}" data-display="1"></div>`;
  }
  const label = languageLabel(lang);
  return (
    `<figure class="md-code" data-lang="${escapeHtml(lang)}">` +
    `<figcaption><span>${escapeHtml(label)}</span><button class="md-copy" type="button" data-copy>Copy</button></figcaption>` +
    `<pre><code>${highlight(code, lang)}</code></pre>` +
    `</figure>`
  );
}

function splitRow(line: string): string[] {
  const t = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < t.length; i++) {
    if (t[i] === "\\" && t[i + 1] === "|") {
      cur += "|";
      i++;
      continue;
    }
    if (t[i] === "|") {
      cells.push(cur.trim());
      cur = "";
      continue;
    }
    cur += t[i];
  }
  cells.push(cur.trim());
  return cells;
}

function alignOf(spec: string): string {
  const left = spec.trim().startsWith(":");
  const right = spec.trim().endsWith(":");
  if (left && right) return "center";
  if (right) return "right";
  return "left";
}

function renderTable(header: string[], aligns: string[], rows: string[][], opts: RenderOptions): string {
  const th = header
    .map((c, i) => `<th style="text-align:${aligns[i] ?? "left"}">${inline(c, opts)}</th>`)
    .join("");
  const body = rows
    .map(
      (r) =>
        `<tr>${header
          .map((_, i) => `<td style="text-align:${aligns[i] ?? "left"}">${inline(r[i] ?? "", opts)}</td>`)
          .join("")}</tr>`,
    )
    .join("");
  return `<div class="md-table-wrap"><table><thead><tr>${th}</tr></thead><tbody>${body}</tbody></table></div>`;
}

/** Renders one list, recursing for nested sub-lists, starting at `start`. */
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: list nesting rules are inherently branchy
function renderList(lines: string[], start: number, opts: RenderOptions): [string, number] {
  const first = lines[start]!;
  const firstMatch = (first.match(UL) ?? first.match(OL))!;
  const ordered = !UL.test(first);
  const baseIndent = firstMatch[1]!.length;

  const items: string[] = [];
  let i = start;

  while (i < lines.length) {
    const line = lines[i]!;
    if (line.trim() === "") {
      // A blank line ends the list unless the next line continues it.
      const next = lines[i + 1];
      if (!next || (!UL.test(next) && !OL.test(next) && next.search(/\S/) <= baseIndent)) break;
      i++;
      continue;
    }
    const m = line.match(UL) ?? line.match(OL);
    if (!m) break;
    const indent = m[1]!.length;
    if (indent < baseIndent) break;

    if (indent > baseIndent) {
      const [nested, next] = renderList(lines, i, opts);
      if (items.length) items[items.length - 1] += nested;
      i = next;
      continue;
    }

    const isOrdered = !UL.test(line);
    if (isOrdered !== ordered) break;

    // Task list checkboxes read much better as real controls.
    let text = m[3]!;
    let prefix = "";
    const task = text.match(/^\[([ xX])\]\s+(.*)$/);
    if (task) {
      prefix = `<input type="checkbox" disabled${task[1] !== " " ? " checked" : ""}> `;
      text = task[2]!;
    }

    // Absorb lazy continuation lines belonging to this item.
    const cont: string[] = [text];
    i++;
    while (i < lines.length) {
      const l = lines[i]!;
      if (l.trim() === "" || UL.test(l) || OL.test(l)) break;
      if (l.search(/\S/) <= baseIndent) break;
      cont.push(l.trim());
      i++;
    }
    items.push(`<li${task ? ' class="md-task"' : ""}>${prefix}${inline(cont.join(" "), opts)}`);
  }

  const tag = ordered ? "ol" : "ul";
  const startAttr = ordered && firstMatch[2] !== "1" ? ` start="${Number(firstMatch[2])}"` : "";
  return [`<${tag}${startAttr}>${items.map((it) => `${it}</li>`).join("")}</${tag}>`, i];
}

// ------------------------------------------------------------------- inline

const H0 = "\x01";
const H1 = "\x02";
// biome-ignore lint/suspicious/noControlCharactersInRegex: sentinels are control chars by design
const HOLE_RE = /\x01(\d+)\x02/g;

/**
 * Inline formatting.
 *
 * Code spans, maths and links are extracted first and replaced with sentinels,
 * so emphasis parsing can never reach inside them — the usual source of mangled
 * output when a code span contains `*` or `_`. The sentinels are control
 * characters, which cannot occur in the source text.
 */
function inline(src: string, opts: RenderOptions): string {
  const holes: string[] = [];
  const hole = (html: string) => {
    holes.push(html);
    return `${H0}${holes.length - 1}${H1}`;
  };

  // Strip any stray sentinels from the input so they cannot forge a hole.
  // biome-ignore lint/suspicious/noControlCharactersInRegex: stripping the sentinels the renderer reserves
  let s = src.replace(/[\x01\x02]/g, "");

  // Inline code
  s = s.replace(/(`+)([\s\S]*?)\1/g, (_m, _t: string, code: string) =>
    hole(`<code>${escapeHtml(code.trim())}</code>`),
  );

  if (opts.math !== false) {
    // Display maths first so $$...$$ is not eaten by the inline rule.
    s = s.replace(/\$\$([\s\S]+?)\$\$/g, (_m, tex: string) =>
      hole(`<span class="md-math" data-tex="${escapeHtml(tex)}" data-display="1"></span>`),
    );
    s = s.replace(/(?<![\\$\w])\$(?!\s)([^$\n]+?)(?<!\s)\$(?![\w$])/g, (_m, tex: string) =>
      hole(`<span class="md-math" data-tex="${escapeHtml(tex)}"></span>`),
    );
    s = s.replace(/\\\[([\s\S]+?)\\\]/g, (_m, tex: string) =>
      hole(`<span class="md-math" data-tex="${escapeHtml(tex)}" data-display="1"></span>`),
    );
    s = s.replace(/\\\(([\s\S]+?)\\\)/g, (_m, tex: string) =>
      hole(`<span class="md-math" data-tex="${escapeHtml(tex)}"></span>`),
    );
  }

  // Images before links: the syntax differs only by a leading '!'.
  s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g, (m, alt: string, url: string) =>
    safeUrl(url) ? hole(`<img src="${escapeHtml(url)}" alt="${escapeHtml(alt)}" loading="lazy">`) : m,
  );

  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g, (m, text: string, url: string) =>
    safeUrl(url) ? hole(`<a href="${escapeHtml(url)}" data-external>${escapeHtml(text)}</a>`) : m,
  );

  // Bare URLs
  s = s.replace(
    /(^|[\s(])(https?:\/\/[^\s<>()]+[^\s<>().,;:!?])/g,
    (_m, pre: string, url: string) =>
      pre + hole(`<a href="${escapeHtml(url)}" data-external>${escapeHtml(url)}</a>`),
  );

  // `path/to/file.ext:42` -- clickable throughout the app.
  s = s.replace(
    /(^|[\s(`"'])((?:~|\.{1,2})?\/?(?:[\w.@-]+\/)+[\w.@-]+\.[A-Za-z]\w{0,9})(?::(\d+))?(?=$|[\s),.:;"'`])/g,
    (_m, pre: string, path: string, lineNo?: string) =>
      pre +
      hole(
        `<a class="md-path" data-path="${escapeHtml(path)}"${lineNo ? ` data-line="${lineNo}"` : ""}>` +
          `${escapeHtml(path)}${lineNo ? `:${lineNo}` : ""}</a>`,
      ),
  );

  s = escapeHtml(s);

  // Emphasis. Strong before em so `***x***` nests correctly.
  s = s.replace(/(\*\*\*|___)(?=\S)([\s\S]*?\S)\1/g, "<strong><em>$2</em></strong>");
  s = s.replace(/(\*\*|__)(?=\S)([\s\S]*?\S)\1/g, "<strong>$2</strong>");
  s = s.replace(/(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])/g, "<em>$1</em>");
  s = s.replace(/(?<![\w_])_(?=\S)([^_\n]*?\S)_(?![\w_])/g, "<em>$1</em>");
  s = s.replace(/~~(?=\S)([\s\S]*?\S)~~/g, "<del>$1</del>");

  // Hard line breaks
  s = s.replace(/ {2,}\n/g, "<br>").replace(/\n/g, "<br>");

  return s.replace(HOLE_RE, (_m, n: string) => holes[Number(n)] ?? "");
}

function safeUrl(url: string): boolean {
  const u = url.trim().toLowerCase();
  return (
    u.startsWith("http://") ||
    u.startsWith("https://") ||
    u.startsWith("mailto:") ||
    u.startsWith("#") ||
    u.startsWith("/") ||
    u.startsWith("./") ||
    u.startsWith("../") ||
    u.startsWith("data:image/") ||
    u.startsWith("asset:") ||
    u.startsWith("http://asset.localhost")
  );
}

/** True when the source contains anything needing a lazy-loaded renderer. */
export function needsMermaid(src: string): boolean {
  return /```\s*(mermaid|flowchart)/.test(src);
}

export function needsMath(src: string): boolean {
  if (/\$\$[\s\S]+?\$\$/.test(src)) return true;
  if (/\\\(|\\\[/.test(src)) return true;
  if (/```\s*(math|latex|tex)/.test(src)) return true;
  return /(?<![\\$\w])\$(?!\s)[^$\n]+\$/.test(src);
}
