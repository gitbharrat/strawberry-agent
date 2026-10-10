// A small, allocation-light syntax highlighter.
//
// Shiki and Prism are both heavier than this entire application. Chat code
// blocks need enough colour to read structure at a glance, not a full grammar,
// so this tokenises the four things that actually carry meaning — comments,
// strings, numbers and keywords — plus a couple of language-specific extras.

const BASE = {
  rust: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while",
  typescript:
    "abstract any as asserts async await boolean break case catch class const constructor continue declare default delete do else enum export extends false finally for from function get if implements import in infer instanceof interface is keyof let new null number object of private protected public readonly return satisfies set static string super switch symbol this throw true try type typeof undefined union unknown var void while yield",
  javascript:
    "async await break case catch class const constructor continue default delete do else export extends false finally for from function get if import in instanceof let new null of return set static super switch this throw true try typeof undefined var void while yield",
  python:
    "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield match case",
  go: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false",
  c: "auto break case char const continue default do double else enum extern float for goto if int long register return short signed sizeof static struct switch typedef union unsigned void volatile while",
  java: "abstract boolean break byte case catch char class const continue default do double else enum extends final finally float for if implements import instanceof int interface long native new package private protected public return short static super switch synchronized this throw throws try void volatile while true false null var record sealed",
  ruby: "alias and begin break case class def defined do else elsif end ensure false for if in module next nil not or redo rescue retry return self super then true undef unless until when while yield",
  php: "abstract and array as break callable case catch class clone const continue declare default do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends final finally fn for foreach function global if implements include instanceof insteadof interface isset list match namespace new or print private protected public readonly require return static switch throw trait try unset use var while yield true false null",
  bash: "if then else elif fi for while until do done case esac function return in select time break continue local export readonly declare source alias unset trap exit",
  sql: "select from where insert update delete into values set join left right inner outer full on group by order having limit offset union all as distinct create table alter drop index primary key foreign references null not and or in exists between like case when then else end",
  css: "",
  json: "",
  yaml: "",
  toml: "",
  html: "",
  markdown: "",
  text: "",
};

const KEYWORDS: Record<string, string> = {
  ...BASE,
  tsx: BASE.typescript,
  jsx: BASE.javascript,
  cpp: `${BASE.c} bool catch class constexpr delete explicit friend inline mutable namespace new noexcept nullptr operator override private protected public template this throw try typename using virtual`,
  csharp: BASE.java,
  kotlin:
    "as break by class continue do else false for fun if in interface is null object package return super this throw true try typealias val var when while suspend data sealed",
  swift:
    "associatedtype class deinit enum extension func import init let protocol struct subscript typealias var break case continue default defer do else fallthrough for guard if in repeat return switch where while as catch false is nil rethrows self super throw throws true try",
  zig: "const var fn pub if else while for switch return struct enum union error try catch defer comptime inline export test true false null undefined",
  lua: "and break do else elseif end false for function goto if in local nil not or repeat return then true until while",
  dart: BASE.java,
  scss: BASE.css,
  vue: BASE.html,
  svelte: BASE.html,
  shell: BASE.bash,
  sh: BASE.bash,
  zsh: BASE.bash,
};

const LINE_COMMENT: Record<string, string> = {
  rust: "//",
  typescript: "//",
  tsx: "//",
  javascript: "//",
  jsx: "//",
  go: "//",
  c: "//",
  cpp: "//",
  java: "//",
  csharp: "//",
  kotlin: "//",
  swift: "//",
  zig: "//",
  dart: "//",
  scss: "//",
  php: "//",
  python: "#",
  ruby: "#",
  bash: "#",
  sh: "#",
  zsh: "#",
  shell: "#",
  yaml: "#",
  toml: "#",
  ini: "#",
  makefile: "#",
  docker: "#",
  ignore: "#",
  sql: "--",
  lua: "--",
};

const BLOCK_COMMENT: Record<string, [string, string]> = {
  rust: ["/*", "*/"],
  typescript: ["/*", "*/"],
  tsx: ["/*", "*/"],
  javascript: ["/*", "*/"],
  jsx: ["/*", "*/"],
  go: ["/*", "*/"],
  c: ["/*", "*/"],
  cpp: ["/*", "*/"],
  java: ["/*", "*/"],
  csharp: ["/*", "*/"],
  kotlin: ["/*", "*/"],
  swift: ["/*", "*/"],
  css: ["/*", "*/"],
  scss: ["/*", "*/"],
  php: ["/*", "*/"],
  sql: ["/*", "*/"],
  dart: ["/*", "*/"],
  html: ["<!--", "-->"],
  xml: ["<!--", "-->"],
  vue: ["<!--", "-->"],
  svelte: ["<!--", "-->"],
  markdown: ["<!--", "-->"],
  python: ['"""', '"""'],
};

const keywordSets = new Map<string, Set<string>>();
function keywordsFor(lang: string): Set<string> {
  let set = keywordSets.get(lang);
  if (!set) {
    set = new Set((KEYWORDS[lang] ?? "").split(/\s+/).filter(Boolean));
    keywordSets.set(lang, set);
  }
  return set;
}

/** Fence aliases people actually type, mapped to the canonical language id. */
const ALIASES: Record<string, string> = {
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  rs: "rust",
  rb: "ruby",
  yml: "yaml",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  console: "bash",
  golang: "go",
  "c++": "cpp",
  "c#": "csharp",
  cs: "csharp",
  kt: "kotlin",
  md: "markdown",
  plaintext: "text",
  txt: "text",
  "": "text",
};

export function canonicalLanguage(lang: string): string {
  const l = (lang || "").toLowerCase();
  return ALIASES[l] ?? l;
}

export function escapeHtml(s: string): string {
  // Charwise beats a chained .replace() here because the common case is a
  // string with nothing to escape at all.
  let out = "";
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    let rep: string | null = null;
    if (c === 38) rep = "&amp;";
    else if (c === 60) rep = "&lt;";
    else if (c === 62) rep = "&gt;";
    else if (c === 34) rep = "&quot;";
    if (rep) {
      out += s.slice(start, i) + rep;
      start = i + 1;
    }
  }
  return start === 0 ? s : out + s.slice(start);
}

const IDENT_START = /[A-Za-z_$@]/;
const IDENT = /[A-Za-z0-9_$]/;

/**
 * Returns HTML with `<span class="tk-*">` wrappers. The input is escaped here,
 * so callers must not escape it first.
 */
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: a tokeniser loop; each branch is one token class
export function highlight(code: string, langRaw: string): string {
  const lang = canonicalLanguage(langRaw) || "text";
  if (lang === "text" || lang === "" || lang === "plain") return escapeHtml(code);
  if (lang === "diff") return highlightDiff(code);

  const kw = keywordsFor(lang);
  const line = LINE_COMMENT[lang];
  const block = BLOCK_COMMENT[lang];
  const isMarkup = lang === "html" || lang === "xml" || lang === "vue" || lang === "svelte";

  let out = "";
  let i = 0;
  const n = code.length;

  const push = (cls: string, text: string) => {
    out += cls ? `<span class="tk-${cls}">${escapeHtml(text)}</span>` : escapeHtml(text);
  };

  while (i < n) {
    const c = code[i]!;

    // Comments
    if (line && code.startsWith(line, i)) {
      const end = code.indexOf("\n", i);
      const stop = end === -1 ? n : end;
      push("cmt", code.slice(i, stop));
      i = stop;
      continue;
    }
    if (block && code.startsWith(block[0], i)) {
      const end = code.indexOf(block[1], i + block[0].length);
      const stop = end === -1 ? n : end + block[1].length;
      push("cmt", code.slice(i, stop));
      i = stop;
      continue;
    }

    // Strings, including template literals with no interpolation handling —
    // colouring the whole literal is closer to right than colouring none of it.
    if (c === '"' || c === "'" || c === "`") {
      let j = i + 1;
      while (j < n) {
        if (code[j] === "\\") {
          j += 2;
          continue;
        }
        if (code[j] === c) {
          j++;
          break;
        }
        if (code[j] === "\n" && c !== "`") break;
        j++;
      }
      push("str", code.slice(i, j));
      i = j;
      continue;
    }

    // Markup tags
    if (isMarkup && c === "<") {
      const j = code.indexOf(">", i);
      const stop = j === -1 ? n : j + 1;
      push("tag", code.slice(i, stop));
      i = stop;
      continue;
    }

    // Numbers
    if (c >= "0" && c <= "9") {
      let j = i;
      while (j < n && /[0-9a-fA-FxXbBoO._]/.test(code[j]!)) j++;
      push("num", code.slice(i, j));
      i = j;
      continue;
    }

    // Identifiers, keywords and call sites
    if (IDENT_START.test(c)) {
      let j = i + 1;
      while (j < n && IDENT.test(code[j]!)) j++;
      const word = code.slice(i, j);
      if (kw.has(word)) push("kw", word);
      else if (code[j] === "(") push("fn", word);
      else if (/^[A-Z]/.test(word) && word.length > 1) push("type", word);
      else push("", word);
      i = j;
      continue;
    }

    // Everything else: run of punctuation/whitespace in one go.
    let j = i;
    while (j < n && !IDENT_START.test(code[j]!) && !/[0-9"'`]/.test(code[j]!)) {
      if (line && code.startsWith(line, j)) break;
      if (block && code.startsWith(block[0], j)) break;
      if (isMarkup && code[j] === "<") break;
      j++;
    }
    if (j === i) j++;
    const chunk = code.slice(i, j);
    push(/^[\s]+$/.test(chunk) ? "" : "op", chunk);
    i = j;
  }

  return out;
}

function highlightDiff(code: string): string {
  return code
    .split("\n")
    .map((l) => {
      if (l.startsWith("+++") || l.startsWith("---")) return `<span class="tk-cmt">${escapeHtml(l)}</span>`;
      if (l.startsWith("@@")) return `<span class="tk-kw">${escapeHtml(l)}</span>`;
      if (l.startsWith("+")) return `<span class="tk-add">${escapeHtml(l)}</span>`;
      if (l.startsWith("-")) return `<span class="tk-del">${escapeHtml(l)}</span>`;
      return escapeHtml(l);
    })
    .join("\n");
}

/** Human label for the code-block header. */
export function languageLabel(langRaw: string): string {
  const lang = canonicalLanguage(langRaw);
  const map: Record<string, string> = {
    typescript: "TypeScript",
    tsx: "TSX",
    javascript: "JavaScript",
    jsx: "JSX",
    rust: "Rust",
    python: "Python",
    go: "Go",
    csharp: "C#",
    cpp: "C++",
    c: "C",
    bash: "Shell",
    sh: "Shell",
    zsh: "Shell",
    shell: "Shell",
    json: "JSON",
    yaml: "YAML",
    toml: "TOML",
    sql: "SQL",
    html: "HTML",
    css: "CSS",
    scss: "SCSS",
    markdown: "Markdown",
    xml: "XML",
    diff: "Diff",
    text: "Text",
  };
  return map[lang] ?? (lang ? lang[0]!.toUpperCase() + lang.slice(1) : "Text");
}
