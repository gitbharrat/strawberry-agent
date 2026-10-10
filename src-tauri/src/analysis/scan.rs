//! Static codebase scanner.
//!
//! Everything here is deterministic and costs zero tokens: files, the import
//! graph between them, the functions inside them, and a heuristic quality
//! read on each. The model only ever adds the short descriptions on top —
//! so a re-scan is always cheap, and the expensive part is incremental.

use super::{ArchEdge, ArchFile, ArchFn};
use regex_lite::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Hard caps so a monorepo cannot wedge the app. Past these the map is
/// truncated, not wrong: files are visited in walk order.
pub(super) const MAX_FILES: usize = 2500;
pub(super) const MAX_FILE_BYTES: u64 = 400 * 1024;
const MAX_FNS_PER_FILE: usize = 120;
const MAX_TOKENS_PER_FN: usize = 24;
const MAX_CALLS_PER_FN: usize = 18;

/// Every extension `Lang::of` accepts, for callers that filter file lists
/// before the texts exist locally (the remote gather builds its shell filter
/// from this). A test keeps it and `Lang::of` in agreement.
pub(super) const SOURCE_EXTS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte", "rs", "py", "go", "java", "kt", "swift",
    "cs", "scala", "c", "h", "cc", "cpp", "hpp", "m", "mm", "rb", "css", "scss", "less",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    JsTs,
    Rust,
    Python,
    Go,
    CLike,
    Ruby,
    Css,
}

impl Lang {
    fn of(ext: &str) -> Option<Self> {
        Some(match ext {
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => Self::JsTs,
            "rs" => Self::Rust,
            "py" => Self::Python,
            "go" => Self::Go,
            "java" | "kt" | "swift" | "cs" | "scala" | "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" => {
                Self::CLike
            }
            "rb" => Self::Ruby,
            "css" | "scss" | "less" => Self::Css,
            _ => return None,
        })
    }

    fn name(self, ext: &str) -> String {
        match self {
            Self::JsTs => match ext {
                "tsx" => "tsx",
                "jsx" => "jsx",
                "js" | "mjs" | "cjs" => "js",
                "vue" => "vue",
                "svelte" => "svelte",
                _ => "ts",
            }
            .into(),
            Self::Rust => "rust".into(),
            Self::Python => "python".into(),
            Self::Go => "go".into(),
            Self::CLike => ext.into(),
            Self::Ruby => "ruby".into(),
            Self::Css => "css".into(),
        }
    }
}

pub struct RawScan {
    pub files: Vec<ArchFile>,
    pub edges: Vec<ArchEdge>,
}

/// A source file already in memory — how remote workspaces arrive, their
/// texts fetched over the wire instead of read from disk.
pub struct SourceFile {
    /// Workspace-relative path.
    pub rel: String,
    pub ext: String,
    pub text: String,
}

/// Scans a workspace into files, functions and an import graph. Pure CPU; the
/// caller wraps it in `spawn_blocking`.
pub fn scan_workspace(root: &Path) -> RawScan {
    let sources = collect_files(root);

    let mut files: Vec<FileScan> = Vec::with_capacity(sources.len());
    for (abs, rel, lang, ext) in sources {
        if let Ok(bytes) = std::fs::read(&abs) {
            if bytes.contains(&0) {
                continue; // binary that slipped past the extension filter
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            files.push(scan_file(rel, lang, &ext, &text));
        }
    }

    link(files)
}

/// Same scan over sources that never touch this machine's disk. The texts are
/// borrowed, so the caller keeps them for the enrichment pass afterwards.
pub fn scan_sources(sources: &[SourceFile]) -> RawScan {
    let mut files: Vec<FileScan> = Vec::with_capacity(sources.len());
    for s in sources {
        let Some(lang) = Lang::of(&s.ext) else { continue };
        if s.text.contains('\0') {
            continue; // binary that slipped past the extension filter
        }
        files.push(scan_file(s.rel.clone(), lang, &s.ext, &s.text));
    }
    link(files)
}

// ------------------------------------------------------------------- walking

/// Directories that are dependency or build output in any ecosystem. The
/// walker honours `.gitignore`, but a workspace that is not a git repo (or has
/// no ignore file) would otherwise drown the map in `node_modules` — the file
/// cap fills with vendored code before a single real source file is reached.
/// The remote gather builds its shell excludes from the same list.
pub(super) const VENDORED_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "dist",
    "build",
    "out",
    "target",
    "vendor",
    "venv",
    ".venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "coverage",
    "Pods",
    "DerivedData",
    ".gradle",
    ".terraform",
];

fn vendored_dir(name: &str) -> bool {
    VENDORED_DIRS.contains(&name)
}

fn collect_files(root: &Path) -> Vec<(PathBuf, String, Lang, String)> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        // Honour .gitignore files even when the folder is not a git repo yet.
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            !(is_dir && e.file_name().to_str().is_some_and(vendored_dir))
        })
        .build();

    for entry in walker.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        // The map must never analyse its own output, and minified or generated
        // bundles only produce noise.
        if rel.starts_with(".strawberry/")
            || rel.contains(".min.")
            || rel.ends_with(".d.ts")
            || rel.contains("/gen/")
        {
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let Some(lang) = Lang::of(&ext) else { continue };
        if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
            continue;
        }
        out.push((path.to_path_buf(), rel, lang, ext));
    }
    out
}

// ------------------------------------------------------------ per-file scan

struct FileScan {
    rel: String,
    lang: Lang,
    lang_name: String,
    lines: u32,
    hash: String,
    imports_raw: Vec<(String, u32)>, // (specifier, named-symbol count)
    functions: Vec<FnScan>,
    /// Every identifier in the file with its occurrence count. A function
    /// passed as a value (a JSX handler, a callback) has no call parens, but
    /// it does appear here more often than its own declaration.
    ident_counts: HashMap<String, u32>,
    /// Service channels this file provides: bound routes, subscribed topics.
    provides: Vec<String>,
    /// Service channels this file consumes: called routes, published events.
    consumes: Vec<String>,
}

struct FnScan {
    name: String,
    line: u32,
    end_line: u32,
    params: String,
    exported: bool,
    complexity: u32,
    nesting: u32,
    length: u32,
    has_todo: bool,
    hint: Option<String>,
    calls: Vec<String>,
    tokens: Vec<String>,
    hash: String,
}

struct Rx {
    js_import: Regex,
    js_named: Regex,
    rs_use: Regex,
    py_import: Regex,
    go_import: Regex,
    c_include: Regex,
    css_import: Regex,
    js_fn: Regex,
    js_arrow: Regex,
    js_method: Regex,
    rs_fn: Regex,
    py_fn: Regex,
    go_fn: Regex,
    c_fn: Regex,
    rb_fn: Regex,
    ident: Regex,
    call: Regex,
    branch: Regex,
    math: Regex,
    route: Regex,
    route_def: Regex,
    route_call: Regex,
    url_call: Regex,
    chan_pub: Regex,
    chan_sub: Regex,
}

fn rx() -> &'static Rx {
    static RX: OnceLock<Rx> = OnceLock::new();
    RX.get_or_init(|| {
        let new = |p: &str| Regex::new(p).unwrap_or_else(|_| Regex::new("$^").expect("empty regex"));
        Rx {
            js_import: new(
                r#"(?:import|export)\s+[^;'"]*?from\s*['"]([^'"]+)['"]|import\s*\(?\s*['"]([^'"]+)['"]|require\s*\(\s*['"]([^'"]+)['"]"#,
            ),
            js_named: new(r"\{([^}]*)\}"),
            rs_use: new(r"^\s*(?:pub\s+)?(?:use|mod)\s+([A-Za-z_][\w:]*)"),
            py_import: new(r"^\s*(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))"),
            go_import: new(r#""([^"]+)""#),
            c_include: new(r#"^\s*#include\s*"([^"]+)""#),
            css_import: new(r#"@import\s+(?:url\()?['"]?([^'")]+)"#),
            js_fn: new(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)\s*\(([^)]*)"),
            js_arrow: new(
                r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)(?:\s*:[^=]+)?\s*=\s*(?:async\s*)?(?:\(([^)]*)\)|([A-Za-z_$][\w$]*))\s*(?::[^=>{]+)?=>",
            ),
            js_method: new(r"^\s*(?:public\s+|private\s+|protected\s+|static\s+|override\s+)*(?:async\s+)?([A-Za-z_$][\w$]*)\s*\(([^)]*)\)\s*(?::[^{;]+)?\{"),
            rs_fn: new(r"^\s*(pub[\s(])?\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)\s*(?:<[^>]*>)?\s*\(([^)]*)"),
            py_fn: new(r"^(\s*)(?:async\s+)?def\s+([A-Za-z_]\w*)\s*\(([^)]*)"),
            go_fn: new(r"^func\s+(?:\([^)]*\)\s+)?([A-Za-z_]\w*)\s*\(([^)]*)"),
            c_fn: new(r"^\s*(?:[\w:<>,~&*\[\]]+\s+)+([A-Za-z_]\w*)\s*\(([^;{]*)\)\s*(?:const\s*)?\{"),
            rb_fn: new(r"^(\s*)def\s+(?:self\.)?([A-Za-z_]\w*[?!]?)\s*(?:\(([^)]*)\))?"),
            ident: new(r"[A-Za-z_$][\w$]{2,}"),
            call: new(r"([A-Za-z_$][\w$]{1,})\s*\("),
            branch: new(r"\b(?:if|elif|else if|for|while|match|switch|case|when|catch|except|guard)\b|&&|\|\|"),
            math: new(r"Math\.\w+|\b(?:sqrt|pow|abs|floor|ceil|round|sin|cos|tan|atan2|log2|log10|exp|hypot|clamp|lerp|min|max)\s*\(|[-+]?\d+\.\d+|<<|>>|\*\*"),
            // A server framework binding a path: app.get("/x"), router.post,
            // .route("/x", …), @app.route("/x"), #[get("/x")]. Verb methods
            // need a server-shaped receiver (app/router/…) so a client like
            // api.post("/x") reads as a call, not a binding; .route/.use are
            // server-only verbs and accept any receiver, chained included.
            route_def: new(
                r#"(?:\b(?:app|router|server|srv|mux|blueprint|bp|web|site)[\w$]*\.(?:get|post|put|delete|patch|head|options|all)|\.(?:route|use|handle|add_api_route|add_url_rule)|#\[(?:get|post|put|delete|patch)|@[\w.]*(?:route|get|post|put|delete|patch|websocket))\s*[(\[]\s*["'](/[^"'\s]*)["']"#,
            ),
            // A client reaching for a path: fetch("/x"), axios.get("/x"),
            // api.post("/x"), requests.get(f"http://…/x") — any dotted verb
            // counts as a client; the same line matching route_def wins as a
            // server binding instead. A `${BASE}` template prefix and Python's
            // f-strings are skipped over so only the path is captured.
            route_call: new(
                r#"(?:\bfetch|axios(?:\.[a-z]+)?|[\w$]+\.(?:get|post|put|delete|patch)|\brequest)\s*\(\s*f?[`"'](?:\$\{[^}]+\})?((?:https?://[^"'`\s]+)|/[^"'`\s]*)[`"']"#,
            ),
            url_call: new(r#"https?://[^/\s]+(/[^\s"'`?#]*)"#),
            // Event and topic channels. The name must carry a separator so DOM
            // events ("click") never read as a service channel.
            chan_pub: new(
                r#"\b(?:publish|emit|produce|dispatchEvent|dispatch|sendEvent|trigger)\w*\s*\(\s*(?:new\s+CustomEvent\s*\(\s*)?["']([\w-]+[:./][\w:./-]+)["']"#,
            ),
            chan_sub: new(
                r#"\b(?:subscribe|consume|addEventListener|addListener|on|once)\s*\(\s*["']([\w-]+[:./][\w:./-]+)["']"#,
            ),
            route: new(r#"['"`]/[\w:{$-]|\broutes?\b|\bpath\b|\bnavigate\b|\bredirect\b|\bdispatch\b|\bhandler\b|\bmatch\s|case\s+['"]"#),
        }
    })
}

/// Keywords that look like calls or method names but are not functions.
fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "while"
            | "switch"
            | "match"
            | "catch"
            | "return"
            | "new"
            | "typeof"
            | "await"
            | "yield"
            | "function"
            | "super"
            | "this"
            | "self"
            | "do"
            | "try"
            | "throw"
            | "case"
            | "when"
            | "loop"
            | "unsafe"
            | "impl"
            | "let"
            | "const"
            | "var"
            | "in"
            | "of"
            | "assert"
            | "print"
            | "println"
            | "sizeof"
            | "defer"
            | "go"
    )
}

/// Strips string contents and trailing comments so brace counting and keyword
/// matching are not fooled by literals. Approximate on purpose — a template
/// literal spanning lines still leaks — but wrong in ways that only cost a
/// point of complexity, never a crash.
fn strip_line(line: &str, lang: Lang) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_str: Option<char> = None;
    while let Some(c) = chars.next() {
        match in_str {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    in_str = None;
                    out.push(q);
                }
            }
            None => match c {
                // In Rust an apostrophe is almost always a lifetime, and
                // treating it as a string opener swallows the rest of the
                // signature — which silently loses the function.
                '\'' if lang == Lang::Rust => out.push(c),
                '"' | '\'' | '`' => {
                    in_str = Some(c);
                    out.push(c);
                }
                '/' if matches!(lang, Lang::JsTs | Lang::Rust | Lang::Go | Lang::CLike | Lang::Css) => {
                    if chars.peek() == Some(&'/') {
                        break; // line comment
                    }
                    out.push(c);
                }
                '#' if matches!(lang, Lang::Python | Lang::Ruby) => break,
                _ => out.push(c),
            },
        }
    }
    out
}

fn scan_file(rel: String, lang: Lang, ext: &str, text: &str) -> FileScan {
    let r = rx();
    let raw_lines: Vec<&str> = text.lines().collect();
    let stripped: Vec<String> = raw_lines.iter().map(|l| strip_line(l, lang)).collect();

    let imports_raw = extract_imports(lang, &raw_lines, r);
    let functions =
        if lang == Lang::Css { Vec::new() } else { extract_functions(lang, &raw_lines, &stripped, r) };

    let mut ident_counts: HashMap<String, u32> = HashMap::new();
    for line in &stripped {
        for m in r.ident.find_iter(line) {
            *ident_counts.entry(m.as_str().to_string()).or_insert(0) += 1;
        }
    }
    let (provides, consumes) = extract_channels(&raw_lines, r);

    FileScan {
        rel,
        lang,
        lang_name: lang.name(ext),
        lines: raw_lines.len() as u32,
        hash: crate::journal::content_hash(text.as_bytes()),
        imports_raw,
        functions,
        ident_counts,
        provides,
        consumes,
    }
}

/// Route parameters vary by framework (`:id`, `{id}`, `<id>`); a wildcard
/// segment lets a definition and a call written differently still meet.
/// Trailing slashes are dropped so `/predict/` and `/predict` agree.
fn normalize_route(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let trimmed = path.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    let segments: Vec<String> = path
        .split('/')
        .map(|s| {
            if s.starts_with(':') || s.starts_with('{') || s.starts_with('<') || s.starts_with('$') {
                "*".to_string()
            } else {
                s.to_ascii_lowercase()
            }
        })
        .collect();
    segments.join("/")
}

/// True when a consumed channel reaches a provided one. Exact label match, or
/// — for routes — the called path ends with the bound path at a segment
/// boundary: a caller going through a gateway prefix (`/api/v1/predict`)
/// still reaches the service that binds `/predict`. The bound side must have
/// at least one concrete segment, so `/` or `/*` never match everything.
fn channel_matches(consume: &str, provide: &str) -> bool {
    if consume == provide {
        return true;
    }
    let (Some(c), Some(p)) = (consume.strip_prefix("route "), provide.strip_prefix("route ")) else {
        return false;
    };
    if !p.starts_with('/') || !p.split('/').any(|s| !s.is_empty() && s != "*") {
        return false;
    }
    c.len() > p.len() && c.ends_with(p)
}

/// Cross-service links that imports cannot see: HTTP routes one side binds
/// and the other calls (across a docker boundary, matched by path string),
/// and pub/sub or custom-event channels matched by topic name.
fn extract_channels(lines: &[&str], r: &Rx) -> (Vec<String>, Vec<String>) {
    const CAP: usize = 24;
    let mut provides: Vec<String> = Vec::new();
    let mut consumes: Vec<String> = Vec::new();
    let push = |list: &mut Vec<String>, entry: String| {
        if list.len() < CAP && !list.contains(&entry) {
            list.push(entry);
        }
    };

    for line in lines {
        // Server bindings on this line win: `app.get("/x", handler)` matches
        // the broadened client pattern too, and must not read as a self-call.
        let mut defs_here: Vec<String> = Vec::new();
        for c in r.route_def.captures_iter(line) {
            if let Some(m) = c.get(1) {
                let label = format!("route {}", normalize_route(m.as_str()));
                defs_here.push(label.clone());
                push(&mut provides, label);
            }
        }
        for c in r.route_call.captures_iter(line) {
            if let Some(m) = c.get(1) {
                let raw = m.as_str();
                let path = if raw.starts_with("http") {
                    match r.url_call.captures(raw).and_then(|u| u.get(1)) {
                        Some(p) => p.as_str(),
                        None => continue,
                    }
                } else {
                    raw
                };
                let label = format!("route {}", normalize_route(path));
                if !defs_here.contains(&label) {
                    push(&mut consumes, label);
                }
            }
        }
        for c in r.chan_pub.captures_iter(line) {
            if let Some(m) = c.get(1) {
                push(&mut consumes, format!("event {}", m.as_str()));
            }
        }
        for c in r.chan_sub.captures_iter(line) {
            if let Some(m) = c.get(1) {
                push(&mut provides, format!("event {}", m.as_str()));
            }
        }
    }
    (provides, consumes)
}

fn extract_imports(lang: Lang, lines: &[&str], r: &Rx) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut push = |spec: &str, named: u32| {
        if spec.is_empty() {
            return;
        }
        match out.iter_mut().find(|(s, _)| s == spec) {
            Some((_, n)) => *n += named.max(1),
            None => out.push((spec.to_string(), named.max(1))),
        }
    };

    // Imports live in the head of a file; scanning 400 lines catches even the
    // longest real import block without walking whole files twice.
    for line in lines.iter().take(400) {
        match lang {
            Lang::JsTs => {
                for c in r.js_import.captures_iter(line) {
                    let spec = c.get(1).or(c.get(2)).or(c.get(3)).map(|m| m.as_str()).unwrap_or("");
                    let named = r
                        .js_named
                        .captures(line)
                        .and_then(|n| n.get(1))
                        .map(|m| m.as_str().split(',').filter(|s| !s.trim().is_empty()).count() as u32)
                        .unwrap_or(1);
                    push(spec, named);
                }
            }
            Lang::Rust => {
                if let Some(c) = r.rs_use.captures(line) {
                    if let Some(m) = c.get(1) {
                        push(m.as_str(), 1);
                    }
                }
            }
            Lang::Python => {
                if let Some(c) = r.py_import.captures(line) {
                    let spec = c.get(1).or(c.get(2)).map(|m| m.as_str()).unwrap_or("");
                    push(spec, 1);
                }
            }
            Lang::Go => {
                let t = line.trim_start();
                if t.starts_with("import") || t.starts_with('"') {
                    for c in r.go_import.captures_iter(line) {
                        if let Some(m) = c.get(1) {
                            push(m.as_str(), 1);
                        }
                    }
                }
            }
            Lang::CLike => {
                if let Some(c) = r.c_include.captures(line) {
                    if let Some(m) = c.get(1) {
                        push(m.as_str(), 1);
                    }
                }
            }
            Lang::Css => {
                for c in r.css_import.captures_iter(line) {
                    if let Some(m) = c.get(1) {
                        push(m.as_str(), 1);
                    }
                }
            }
            Lang::Ruby => {
                if let Some(rest) = line.trim().strip_prefix("require_relative ") {
                    push(rest.trim_matches(['"', '\'', ' ']), 1);
                }
            }
        }
    }
    out
}

/// A detected declaration line: name, params, and whether it opened a brace.
struct Decl {
    name: String,
    params: String,
    exported: bool,
    indent: usize,
}

fn detect_decl(lang: Lang, raw: &str, stripped: &str, r: &Rx) -> Option<Decl> {
    let indent = raw.len() - raw.trim_start().len();
    let exported_js = raw.trim_start().starts_with("export");
    match lang {
        Lang::JsTs => {
            if let Some(c) = r.js_fn.captures(stripped) {
                return Some(Decl {
                    name: c.get(1)?.as_str().into(),
                    params: c.get(2).map(|m| m.as_str().into()).unwrap_or_default(),
                    exported: exported_js,
                    indent,
                });
            }
            if let Some(c) = r.js_arrow.captures(stripped) {
                let params = c.get(2).or(c.get(3)).map(|m| m.as_str().to_string()).unwrap_or_default();
                return Some(Decl { name: c.get(1)?.as_str().into(), params, exported: exported_js, indent });
            }
            // Class methods; the keyword filter is what keeps `if (...) {` out.
            if indent > 0 {
                if let Some(c) = r.js_method.captures(stripped) {
                    let name = c.get(1)?.as_str();
                    if !is_keyword(name) {
                        return Some(Decl {
                            name: name.into(),
                            params: c.get(2).map(|m| m.as_str().into()).unwrap_or_default(),
                            exported: false,
                            indent,
                        });
                    }
                }
            }
            None
        }
        Lang::Rust => r.rs_fn.captures(stripped).map(|c| Decl {
            name: c.get(2).map(|m| m.as_str()).unwrap_or_default().into(),
            params: c.get(3).map(|m| m.as_str().into()).unwrap_or_default(),
            exported: c.get(1).is_some() || stripped.trim_start().starts_with("pub"),
            indent,
        }),
        Lang::Python => r.py_fn.captures(stripped).map(|c| Decl {
            name: c.get(2).map(|m| m.as_str()).unwrap_or_default().into(),
            params: c.get(3).map(|m| m.as_str().into()).unwrap_or_default(),
            exported: !c.get(2).map(|m| m.as_str().starts_with('_')).unwrap_or(false),
            indent: c.get(1).map(|m| m.as_str().len()).unwrap_or(0),
        }),
        Lang::Go => r.go_fn.captures(stripped).map(|c| {
            let name: String = c.get(1).map(|m| m.as_str()).unwrap_or_default().into();
            let exported = name.chars().next().is_some_and(|ch| ch.is_uppercase());
            Decl { name, params: c.get(2).map(|m| m.as_str().into()).unwrap_or_default(), exported, indent }
        }),
        Lang::CLike => {
            let c = r.c_fn.captures(stripped)?;
            let name = c.get(1)?.as_str();
            if is_keyword(name) {
                return None;
            }
            Some(Decl {
                name: name.into(),
                params: c.get(2).map(|m| m.as_str().into()).unwrap_or_default(),
                exported: true,
                indent,
            })
        }
        Lang::Ruby => r.rb_fn.captures(stripped).map(|c| Decl {
            name: c.get(2).map(|m| m.as_str()).unwrap_or_default().into(),
            params: c.get(3).map(|m| m.as_str().into()).unwrap_or_default(),
            exported: true,
            indent: c.get(1).map(|m| m.as_str().len()).unwrap_or(0),
        }),
        Lang::Css => None,
    }
}

/// End of a function starting at `start`: brace-balanced for brace languages,
/// dedent for Python and Ruby.
fn body_end(lang: Lang, stripped: &[String], start: usize, indent: usize) -> usize {
    let n = stripped.len();
    match lang {
        Lang::Python | Lang::Ruby => {
            let mut end = start;
            for (i, line) in stripped.iter().enumerate().take(n).skip(start + 1) {
                let t = line.trim_end();
                if t.trim().is_empty() {
                    continue;
                }
                let ind = t.len() - t.trim_start().len();
                if ind <= indent {
                    break;
                }
                end = i;
            }
            end
        }
        _ => {
            let mut depth: i32 = 0;
            let mut opened = false;
            for (i, line) in stripped.iter().enumerate().take(n).skip(start) {
                for ch in line.chars() {
                    match ch {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if opened && depth <= 0 {
                    return i;
                }
                if !opened {
                    // A bare `;` before any `{` is a signature-only
                    // declaration (trait method, extern) — a one-liner.
                    if line.trim_end().ends_with(';') {
                        return start;
                    }
                    // Multi-line signatures are common in Rust; give the
                    // opening brace a dozen lines to appear before assuming an
                    // expression-bodied one-liner.
                    if i > start + 12 {
                        return start;
                    }
                }
            }
            n.saturating_sub(1)
        }
    }
}

fn extract_functions(lang: Lang, raw: &[&str], stripped: &[String], r: &Rx) -> Vec<FnScan> {
    let mut out: Vec<FnScan> = Vec::new();
    let mut i = 0usize;
    let n = raw.len();

    while i < n && out.len() < MAX_FNS_PER_FILE {
        let Some(decl) = detect_decl(lang, raw[i], &stripped[i], r) else {
            i += 1;
            continue;
        };
        if decl.name.is_empty() {
            i += 1;
            continue;
        }
        let end = body_end(lang, stripped, i, decl.indent).max(i);
        let body: Vec<&str> = stripped[i..=end.min(n - 1)].iter().map(String::as_str).collect();
        out.push(measure(decl, i as u32 + 1, end as u32 + 1, &body, raw[i..=end.min(n - 1)].join("\n"), r));

        // Nested declarations (helpers inside a component) still get found:
        // only advance past the declaration line, not the whole body.
        i += 1;
    }
    out
}

fn measure(decl: Decl, line: u32, end_line: u32, body: &[&str], raw_body: String, r: &Rx) -> FnScan {
    let length = end_line - line + 1;
    let mut complexity = 1u32;
    let mut max_depth = 0i32;
    let mut depth = 0i32;
    let mut math_hits = 0u32;
    let mut route_hits = 0u32;
    let mut has_todo = false;

    let mut token_freq: HashMap<String, u32> = HashMap::new();
    let mut calls: Vec<String> = Vec::new();

    for l in body {
        complexity += r.branch.find_iter(l).count() as u32;
        math_hits += r.math.find_iter(l).count() as u32;
        route_hits += r.route.find_iter(l).count() as u32;
        if l.contains("TODO") || l.contains("FIXME") || l.contains("HACK") {
            has_todo = true;
        }
        for ch in l.chars() {
            match ch {
                '{' | '(' => {
                    depth += 1;
                    max_depth = max_depth.max(depth);
                }
                '}' | ')' => depth -= 1,
                _ => {}
            }
        }
        for m in r.ident.find_iter(l) {
            let t = m.as_str();
            if !is_keyword(t) {
                *token_freq.entry(t.to_ascii_lowercase()).or_insert(0) += 1;
            }
        }
        for c in r.call.captures_iter(l) {
            if let Some(m) = c.get(1) {
                let name = m.as_str();
                if !is_keyword(name) && name != decl.name && !calls.iter().any(|x| x == name) {
                    calls.push(name.to_string());
                    if calls.len() >= MAX_CALLS_PER_FN {
                        break;
                    }
                }
            }
        }
    }

    let mut tokens: Vec<(String, u32)> = token_freq.into_iter().collect();
    tokens.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tokens.truncate(MAX_TOKENS_PER_FN);

    // A hint is only raised when the signal is dense enough to mean something:
    // one sqrt in 80 lines is not a math kernel.
    let per_line = |hits: u32| hits as f32 / length.max(1) as f32;
    let hint = if math_hits >= 5 && per_line(math_hits) > 0.28 {
        Some("math".to_string())
    } else if route_hits >= 5 && per_line(route_hits) > 0.22 {
        Some("routing".to_string())
    } else {
        None
    };

    let nesting = max_depth.max(0) as u32;

    FnScan {
        name: decl.name,
        line,
        end_line,
        params: decl.params.trim().chars().take(120).collect(),
        exported: decl.exported,
        complexity,
        nesting,
        length,
        has_todo,
        hint,
        calls,
        tokens: tokens.into_iter().map(|(t, _)| t).collect(),
        hash: crate::journal::content_hash(raw_body.as_bytes()),
    }
}

/// Heuristic lint pass. The score is a starting point for the eye, not a
/// verdict; each deduction names itself so the UI can show why.
fn grade(length: u32, nesting: u32, complexity: u32, params: &str, has_todo: bool) -> (u8, Vec<String>) {
    let mut score = 100i32;
    let mut lints = Vec::new();
    let param_count = params.split(',').filter(|p| !p.trim().is_empty()).count() as u32;

    if length > 80 {
        score -= ((length - 80) / 4).min(25) as i32;
        lints.push(format!("long: {length} lines"));
    }
    if nesting > 4 {
        score -= (6 * (nesting - 4)).min(20) as i32;
        lints.push(format!("deep nesting: {nesting}"));
    }
    if complexity > 15 {
        score -= (2 * (complexity - 15)).min(30) as i32;
        lints.push(format!("high branching: {complexity}"));
    }
    if param_count > 5 {
        score -= (4 * (param_count - 5)).min(12) as i32;
        lints.push(format!("{param_count} parameters"));
    }
    if has_todo {
        score -= 5;
        lints.push("has TODO/FIXME".into());
    }
    (score.clamp(20, 100) as u8, lints)
}

// -------------------------------------------------------------- graph linking

/// Resolves import specifiers to files in the scan set, weights the edges, and
/// marks cross-file hot functions as core.
fn link(files: Vec<FileScan>) -> RawScan {
    let index: HashMap<String, usize> = files.iter().enumerate().map(|(i, f)| (f.rel.clone(), i)).collect();

    // name -> files that define a function with that name
    let mut def_sites: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for func in &f.functions {
            def_sites.entry(func.name.as_str()).or_default().push(i);
        }
    }

    let mut edge_w: HashMap<(usize, usize), u32> = HashMap::new();
    let mut resolved_imports: Vec<Vec<String>> = vec![Vec::new(); files.len()];
    // Any unambiguous call, intra-file included — decides "is this used at all".
    let mut ref_counts: HashMap<(usize, String), u32> = HashMap::new();
    // Cross-file calls only — decides "is this core".
    let mut cross_refs: HashMap<(usize, String), u32> = HashMap::new();

    for (i, f) in files.iter().enumerate() {
        for (spec, named) in &f.imports_raw {
            if let Some(&j) = resolve_spec(&f.rel, f.lang, spec, &index) {
                if j != i {
                    *edge_w.entry((i, j)).or_insert(0) += named;
                    if !resolved_imports[i].contains(&files[j].rel) {
                        resolved_imports[i].push(files[j].rel.clone());
                    }
                }
            }
        }

        // Call references add weight beyond the import line itself: a file
        // that calls ten of your functions is more coupled than one that
        // imports a type. Intra-file calls carry no edge weight but still
        // count as usage — a private helper is not dead because only its own
        // file calls it.
        for func in &f.functions {
            for call in &func.calls {
                if let Some(sites) = def_sites.get(call.as_str()) {
                    // Only unambiguous calls count; a name defined in five
                    // places says nothing.
                    if sites.len() == 1 {
                        *ref_counts.entry((sites[0], call.clone())).or_insert(0) += 1;
                        if sites[0] != i {
                            *edge_w.entry((i, sites[0])).or_insert(0) += 1;
                            *cross_refs.entry((sites[0], call.clone())).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
    }

    link_channels(&files, &mut edge_w);

    let mut in_degree: Vec<u32> = vec![0; files.len()];
    let mut degree: Vec<u32> = vec![0; files.len()];
    let mut used_by: Vec<Vec<String>> = vec![Vec::new(); files.len()];
    let mut edges: Vec<ArchEdge> = Vec::with_capacity(edge_w.len());
    for ((a, b), w) in &edge_w {
        in_degree[*b] += w;
        degree[*a] += w;
        degree[*b] += w;
        let from = files[*a].rel.clone();
        if !used_by[*b].contains(&from) {
            used_by[*b].push(from);
        }
        edges.push(ArchEdge {
            from: files[*a].rel.clone(),
            to: files[*b].rel.clone(),
            weight: *w,
            via: vias_between(&files[*a], &files[*b]),
        });
    }
    edges.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));
    for list in &mut used_by {
        list.sort();
    }

    // Hubs: the top ~8% of the codebase by total coupling.
    let hub_cut = {
        let mut d: Vec<u32> = degree.clone();
        d.sort_unstable_by(|x, y| y.cmp(x));
        d.get((files.len() as f32 * 0.08) as usize).copied().unwrap_or(u32::MAX).max(1)
    };

    let mut out_files: Vec<ArchFile> = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let fns: Vec<ArchFn> = f
                .functions
                .iter()
                .map(|s| {
                    let used = ref_counts.get(&(i, s.name.clone())).copied().unwrap_or(0);
                    let xrefs = cross_refs.get(&(i, s.name.clone())).copied().unwrap_or(0);
                    // Dead-code candidate: file-private, never called
                    // anywhere, never even mentioned as a value in its own
                    // file beyond its declaration (a JSX handler or callback
                    // is passed without parens and must not read as dead),
                    // and unique enough that the silence means something.
                    let unambiguous = def_sites.get(s.name.as_str()).map(|v| v.len() == 1).unwrap_or(false);
                    let mentions = f.ident_counts.get(&s.name).copied().unwrap_or(0);
                    let dead = !s.exported
                        && used == 0
                        && mentions <= 1
                        && unambiguous
                        && !matches!(s.name.as_str(), "main" | "constructor");

                    let (mut quality, mut lints) =
                        grade(s.length, s.nesting, s.complexity, &s.params, s.has_todo);
                    if dead {
                        quality = quality.saturating_sub(12).max(20);
                        lints.push("possibly unused: no references found".into());
                    }
                    ArchFn {
                        name: s.name.clone(),
                        line: s.line,
                        end_line: s.end_line,
                        params: s.params.clone(),
                        exported: s.exported,
                        complexity: s.complexity,
                        nesting: s.nesting,
                        quality,
                        lints,
                        calls: s.calls.clone(),
                        tokens: s.tokens.clone(),
                        hash: s.hash.clone(),
                        hint: s.hint.clone(),
                        core: xrefs >= 3 || (s.exported && xrefs >= 2),
                        dead,
                        stale: true,
                        desc: None,
                        inputs: None,
                        outputs: None,
                        tag: None,
                        note: None,
                        big_o: None,
                        space: None,
                        similar: Vec::new(),
                        log: Vec::new(),
                    }
                })
                .collect();

            let (fq, mut fl) = grade_file(f, &fns);
            // A file nothing imports is either an entry point or drift.
            let base = f.rel.rsplit('/').next().unwrap_or(&f.rel).to_ascii_lowercase();
            let entryish = base.starts_with("main.")
                || base.starts_with("index.")
                || base.starts_with("app.")
                || base == "mod.rs"
                || base == "lib.rs"
                || base == "__init__.py";
            if in_degree[i] == 0 && !entryish && files.len() > 3 {
                fl.push("not imported by any scanned file".into());
            }
            ArchFile {
                id: f.rel.clone(),
                lang: f.lang_name.clone(),
                lines: f.lines,
                hash: f.hash.clone(),
                quality: fq,
                lints: fl,
                role: None,
                kind: classify_file(f, in_degree[i], &fns).map(str::to_owned),
                imports: resolved_imports[i].clone(),
                provides: f.provides.clone(),
                consumes: f.consumes.clone(),
                used_by: used_by[i].clone(),
                in_degree: in_degree[i],
                degree: degree[i],
                hub: degree[i] > 0 && degree[i] >= hub_cut,
                max_cx: fns.iter().map(|x| x.complexity).max().unwrap_or(0),
                functions: fns,
                log: Vec::new(),
            }
        })
        .collect();

    compute_similarity(&mut out_files);

    RawScan { files: out_files, edges }
}

/// Connects channel consumers to providers: the frontend that fetches
/// `/api/alpha` gains an edge to the backend that binds it, publishers to
/// subscribers — links imports cannot see, docker boundary or not.
fn link_channels(files: &[FileScan], edge_w: &mut HashMap<(usize, usize), u32>) {
    let mut providers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for p in &f.provides {
            providers.entry(p.as_str()).or_default().push(i);
        }
    }
    // Route labels flattened for the suffix fallback; unique labels stay few
    // even in big maps, so the scan per unmatched consume is cheap.
    let route_providers: Vec<(&str, usize)> = files
        .iter()
        .enumerate()
        .flat_map(|(i, f)| {
            f.provides.iter().filter(|p| p.starts_with("route ")).map(move |p| (p.as_str(), i))
        })
        .collect();
    for (i, f) in files.iter().enumerate() {
        for c in &f.consumes {
            let mut hit = false;
            if let Some(sites) = providers.get(c.as_str()) {
                for &j in sites {
                    if j != i {
                        *edge_w.entry((i, j)).or_insert(0) += 2;
                        hit = true;
                    }
                }
            }
            if !hit && c.starts_with("route ") {
                for (p, j) in &route_providers {
                    if *j != i && channel_matches(c, p) {
                        *edge_w.entry((i, *j)).or_insert(0) += 2;
                    }
                }
            }
        }
    }
}

/// The channel labels for an edge pair, for `ArchEdge.via`.
fn vias_between(a: &FileScan, b: &FileScan) -> Vec<String> {
    a.consumes
        .iter()
        .filter(|c| b.provides.iter().any(|p| channel_matches(c, p)))
        .take(4)
        .cloned()
        .collect()
}

/// Splits an identifier into lowercase words on camelCase and snake_case.
fn name_words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in name.chars() {
        if c == '_' || c == '-' {
            if cur.len() > 1 {
                out.push(cur.to_ascii_lowercase());
            }
            cur = String::new();
        } else if c.is_uppercase() && !cur.is_empty() && cur.chars().last().is_some_and(char::is_lowercase) {
            if cur.len() > 1 {
                out.push(cur.to_ascii_lowercase());
            }
            cur = c.to_string();
        } else {
            cur.push(c);
        }
    }
    if cur.len() > 1 {
        out.push(cur.to_ascii_lowercase());
    }
    out
}

/// |intersection| / min(len) — generous to a small fingerprint inside a big one.
fn overlap(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set: std::collections::HashSet<&str> = a.iter().map(String::as_str).collect();
    let hits = b.iter().filter(|t| set.contains(t.as_str())).count();
    hits as f32 / a.len().min(b.len()) as f32
}

const SIMILAR_THRESHOLD: f32 = 0.34;
const SIMILAR_KEEP: usize = 6;
/// Past this the O(n²) pass costs real time; big maps keep similarity empty.
const SIMILAR_MAX_FNS: usize = 4000;

/// Precomputes each function's cross-codebase lookalikes so the answer lives
/// in the architecture file rather than being derived at view time.
fn compute_similarity(files: &mut [ArchFile]) {
    let total: usize = files.iter().map(|f| f.functions.len()).sum();
    if total == 0 || total > SIMILAR_MAX_FNS {
        return;
    }

    struct Entry {
        file: usize,
        func: usize,
        words: Vec<String>,
    }
    let mut index: Vec<Entry> = Vec::with_capacity(total);
    for (fi, f) in files.iter().enumerate() {
        for (gi, g) in f.functions.iter().enumerate() {
            index.push(Entry { file: fi, func: gi, words: name_words(&g.name) });
        }
    }

    let mut hits: Vec<Vec<crate::analysis::SimilarRef>> = vec![Vec::new(); total];
    for i in 0..index.len() {
        for j in (i + 1)..index.len() {
            let (a, b) = (&index[i], &index[j]);
            let fa = &files[a.file].functions[a.func];
            let fb = &files[b.file].functions[b.func];
            if a.file == b.file && fa.name == fb.name {
                continue;
            }
            let mut score = 0.5 * overlap(&fa.tokens, &fb.tokens)
                + 0.28 * overlap(&a.words, &b.words)
                + 0.15 * overlap(&fa.calls, &fb.calls);
            if fa.hint.is_some() && fa.hint == fb.hint {
                score += 0.07;
            }
            if score >= SIMILAR_THRESHOLD {
                let ra = format!("{}::{}", files[a.file].id, fa.name);
                let rb = format!("{}::{}", files[b.file].id, fb.name);
                hits[i].push(crate::analysis::SimilarRef { target: rb, score });
                hits[j].push(crate::analysis::SimilarRef { target: ra, score });
            }
        }
    }

    for (slot, entry) in hits.into_iter().zip(index.iter()) {
        let mut slot = slot;
        slot.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
        slot.truncate(SIMILAR_KEEP);
        files[entry.file].functions[entry.func].similar = slot;
    }
}

/// Graph-shape role heuristic, refined later by the model. Confidence over
/// coverage: a file that fits no pattern gets no kind rather than a wrong one.
fn classify_file(f: &FileScan, in_degree: u32, fns: &[ArchFn]) -> Option<&'static str> {
    let base = f.rel.rsplit('/').next().unwrap_or(&f.rel).to_ascii_lowercase();
    let path = f.rel.to_ascii_lowercase();
    let out_degree = f.imports_raw.len() as u32;

    if base.contains(".test.")
        || base.contains(".spec.")
        || base.ends_with("_test.rs")
        || path.contains("/tests/")
    {
        return Some("test");
    }
    if base.contains("config") || base.starts_with(".") {
        return Some("config");
    }
    if base.starts_with("main.") || base.starts_with("index.") || base.starts_with("app.") {
        return Some("entry");
    }
    if base.contains("store")
        || base.contains("state")
        || base.contains("context")
        || base.contains("reducer")
    {
        return Some("state");
    }
    if base.contains("types") || base.contains("model") || base.contains("schema") {
        return Some("model");
    }
    if base.contains("util") || base.contains("helper") || base.contains("common") {
        return Some("util");
    }
    // Component files: UI unless they look like something stronger above.
    if matches!(f.lang_name.as_str(), "tsx" | "jsx" | "vue" | "svelte") {
        return Some("ui");
    }
    // Imports many modules and is imported by few: it coordinates the others.
    if out_degree >= 4 && out_degree > in_degree {
        return Some("orchestrator");
    }
    // Imported everywhere while depending on almost nothing: shared helper.
    if in_degree >= 3 && out_degree <= 1 {
        return Some("util");
    }
    // No functions, mostly declarations.
    if fns.is_empty() && f.lines > 10 {
        return Some("model");
    }
    None
}

fn grade_file(f: &FileScan, fns: &[ArchFn]) -> (u8, Vec<String>) {
    let mut lints = Vec::new();
    let base = if fns.is_empty() {
        85
    } else {
        (fns.iter().map(|x| x.quality as u32).sum::<u32>() / fns.len() as u32) as i32
    };
    let mut score = base;
    if f.lines > 600 {
        score -= 8;
        lints.push(format!("large file: {} lines", f.lines));
    }
    if fns.len() > 40 {
        score -= 6;
        lints.push(format!("{} functions in one file", fns.len()));
    }
    let flagged = fns.iter().filter(|x| !x.lints.is_empty()).count();
    if !fns.is_empty() && flagged * 2 > fns.len() {
        lints.push(format!("{flagged} of {} functions flagged", fns.len()));
    }
    (score.clamp(20, 100) as u8, lints)
}

/// Best-effort resolution of an import specifier to a scanned file.
fn resolve_spec<'a>(
    from: &str,
    lang: Lang,
    spec: &str,
    index: &'a HashMap<String, usize>,
) -> Option<&'a usize> {
    let dir = Path::new(from).parent().unwrap_or_else(|| Path::new(""));

    let candidates: Vec<String> = match lang {
        Lang::JsTs | Lang::Css => {
            let base = if let Some(rest) = spec.strip_prefix("~/") {
                format!("src/{rest}")
            } else if let Some(rest) = spec.strip_prefix("@/") {
                format!("src/{rest}")
            } else if spec.starts_with('.') {
                normalize(dir, spec)
            } else {
                return None; // external package
            };
            let mut c = vec![base.clone()];
            for ext in ["ts", "tsx", "js", "jsx", "mjs", "css", "scss", "vue", "svelte"] {
                c.push(format!("{base}.{ext}"));
                c.push(format!("{base}/index.{ext}"));
            }
            c
        }
        Lang::Rust => {
            let seg = spec
                .strip_prefix("crate::")
                .or_else(|| spec.strip_prefix("self::"))
                .unwrap_or(spec)
                .split("::")
                .next()
                .unwrap_or(spec);
            if seg.is_empty() || seg == "std" || seg == "core" || seg == "alloc" || seg == "super" {
                return None;
            }
            let d = dir.to_string_lossy();
            vec![
                format!("{d}/{seg}.rs"),
                format!("{d}/{seg}/mod.rs"),
                // `use crate::x` from anywhere in the crate: try the file's
                // src root by walking up to a `src` component.
                src_root(from).map(|r| format!("{r}/{seg}.rs")).unwrap_or_default(),
                src_root(from).map(|r| format!("{r}/{seg}/mod.rs")).unwrap_or_default(),
            ]
        }
        Lang::Python => {
            let path = spec.trim_start_matches('.').replace('.', "/");
            let d = dir.to_string_lossy();
            vec![
                format!("{path}.py"),
                format!("{path}/__init__.py"),
                format!("{d}/{path}.py"),
                format!("{d}/{}.py", spec.trim_start_matches('.')),
            ]
        }
        Lang::Go => {
            let seg = spec.rsplit('/').next().unwrap_or(spec);
            index
                .keys()
                .filter(|k| k.ends_with(".go") && Path::new(k).parent().is_some_and(|p| p.ends_with(seg)))
                .take(1)
                .cloned()
                .collect()
        }
        Lang::CLike | Lang::Ruby => vec![normalize(dir, spec), spec.to_string()],
    };

    candidates.iter().filter(|c| !c.is_empty()).find_map(|c| index.get(c.as_str()))
}

/// Nearest `src` directory above a file, as a relative prefix.
fn src_root(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    parts.iter().rposition(|p| *p == "src").map(|i| parts[..=i].join("/"))
}

/// Joins and lexically normalises `dir/spec` (`.`/`..` resolved).
fn normalize(dir: &Path, spec: &str) -> String {
    let dir_str = dir.to_string_lossy();
    let mut parts: Vec<&str> = dir_str.split('/').filter(|s| !s.is_empty()).collect();
    for seg in spec.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn finds_ts_functions_and_imports() {
        let src = r#"import { a, b } from "./other";
import util from "~/lib/util";

export function alpha(x: number, y: number): number {
  if (x > y) {
    return Math.sqrt(x * x + y * y);
  }
  return x + y;
}

const beta = async (q: string) => {
  return q.trim();
};
"#;
        let f = scan_file("src/mod.ts".into(), Lang::JsTs, "ts", src);
        let names: Vec<&str> = f.functions.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "found {names:?}");
        assert!(names.contains(&"beta"), "found {names:?}");
        assert_eq!(f.imports_raw.len(), 2);
        assert!(f.functions[0].exported);
    }

    /// The remote gather filters by SOURCE_EXTS before texts exist locally, so
    /// the list must accept exactly what `Lang::of` accepts.
    #[test]
    fn source_exts_match_lang_of() {
        for ext in SOURCE_EXTS {
            assert!(Lang::of(ext).is_some(), "{ext} listed but Lang::of rejects it");
        }
        for ext in ["ts", "rs", "py", "go", "rb", "css", "cpp", "svelte"] {
            assert!(SOURCE_EXTS.contains(&ext), "{ext} accepted by Lang::of but not listed");
        }
    }

    #[test]
    fn finds_rust_functions() {
        let src = "use crate::db;\n\npub fn visible(a: u32) -> u32 {\n    a + 1\n}\n\nfn hidden() {}\n";
        let f = scan_file("src/x.rs".into(), Lang::Rust, "rs", src);
        assert_eq!(f.functions.len(), 2);
        assert!(f.functions[0].exported);
        assert!(!f.functions[1].exported);
    }

    /// Lifetimes must not read as unterminated char literals, and a brace a
    /// few lines below the `fn` still belongs to it.
    #[test]
    fn rust_lifetimes_and_multiline_signatures_survive() {
        let src = "pub fn borrow<'a>(x: &'a str) -> &'a str {\n    x\n}\n\npub fn long(\n    a: u32,\n    b: u32,\n    c: u32,\n) -> u32 {\n    if a > b {\n        a\n    } else {\n        b + c\n    }\n}\n";
        let f = scan_file("src/l.rs".into(), Lang::Rust, "rs", src);
        let names: Vec<&str> = f.functions.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"borrow"), "lifetime fn missing: {names:?}");
        let long = f.functions.iter().find(|x| x.name == "long").expect("multi-line fn found");
        assert!(long.end_line >= 14, "body not tracked past the signature: ends {}", long.end_line);
        assert!(long.complexity > 1, "branches inside the body were not counted");
    }

    #[test]
    fn keyword_lines_are_not_methods() {
        let src = "class T {\n  run(a) {\n    if (a) {\n      return 1;\n    }\n  }\n}\n";
        let f = scan_file("src/t.ts".into(), Lang::JsTs, "ts", src);
        let names: Vec<&str> = f.functions.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"run"));
        assert!(!names.contains(&"if"));
    }

    /// A handler passed as a value (`onClick={handle}`) is used code; only a
    /// function whose name appears nowhere beyond its declaration is flagged.
    #[test]
    fn dead_code_spares_value_references() {
        let src = "function used() {}\nfunction handle() {}\nfunction trulyDead() {}\nconst el = { onClick: handle };\nused();\n";
        let f = scan_file("src/a.ts".into(), Lang::JsTs, "ts", src);
        let raw = RawScan { files: vec![], edges: vec![] };
        let _ = raw;
        let scan = link(vec![f]);
        let by_name = |n: &str| scan.files[0].functions.iter().find(|x| x.name == n).cloned();
        assert!(!by_name("used").expect("used found").dead);
        assert!(!by_name("handle").expect("handle found").dead, "value reference must count as usage");
        assert!(by_name("trulyDead").expect("trulyDead found").dead, "unreferenced private fn is flagged");
    }

    /// The docker-boundary case: a backend binding /api/alpha and a frontend
    /// fetching it share no import, but the route string links them — and a
    /// custom event channel links its publisher to its subscriber.
    /// The real-world frontend↔backend shapes: a `${BASE}`-templated fetch, a
    /// generic client instance (`api.post`), a Python f-string URL, a FastAPI
    /// decorator, and a gateway prefix on the caller's side.
    #[test]
    fn api_calls_reach_backends_through_bases_and_prefixes() {
        let backend = scan_file(
            "backend/main.py".into(),
            Lang::Python,
            "py",
            "@app.post(\"/predict\")\ndef predict():\n    pass\n",
        );
        let front = scan_file(
            "web/api.ts".into(),
            Lang::JsTs,
            "ts",
            "export function run() {\n  return fetch(`${API_BASE}/predict`, { method: \"POST\" });\n}\nexport function other() {\n  return api.post(\"/api/v1/predict\");\n}\n",
        );
        let script = scan_file(
            "tools/call.py".into(),
            Lang::Python,
            "py",
            "def go():\n    return requests.post(f\"http://{host}:8000/predict\")\n",
        );
        assert!(backend.provides.contains(&"route /predict".to_string()), "{:?}", backend.provides);
        assert!(front.consumes.contains(&"route /predict".to_string()), "{:?}", front.consumes);
        assert!(front.consumes.contains(&"route /api/v1/predict".to_string()), "{:?}", front.consumes);
        assert!(script.consumes.contains(&"route /predict".to_string()), "{:?}", script.consumes);

        let scan = link(vec![backend, front, script]);
        let has = |from: &str, to: &str| {
            scan.edges.iter().any(|e| e.from == from && e.to == to && !e.via.is_empty())
        };
        assert!(has("web/api.ts", "backend/main.py"), "template/prefix edge missing: {:?}", scan.edges);
        assert!(has("tools/call.py", "backend/main.py"), "f-string edge missing: {:?}", scan.edges);
    }

    /// A server binding line must not read as the file calling itself.
    #[test]
    fn server_bindings_do_not_self_consume() {
        let f = scan_file(
            "srv/app.js".into(),
            Lang::JsTs,
            "js",
            "app.get(\"/health\", handler);\n",
        );
        assert!(f.provides.contains(&"route /health".to_string()), "{:?}", f.provides);
        assert!(f.consumes.is_empty(), "self-consume leaked: {:?}", f.consumes);
    }

    #[test]
    fn route_suffix_matching_is_bounded() {
        assert!(channel_matches("route /api/v1/predict", "route /predict"));
        assert!(channel_matches("route /predict", "route /predict"));
        assert!(!channel_matches("route /predict", "route /api/v1/predict"), "no reverse widening");
        assert!(!channel_matches("route /api/x", "route /"), "root must not swallow everything");
        assert!(!channel_matches("route /api/x", "route /*"), "wildcard-only must not match");
        assert!(!channel_matches("event a.b", "route /a.b"), "kinds never mix");
        assert_eq!(normalize_route("/predict/"), "/predict", "trailing slash folds");
    }

    #[test]
    fn routes_and_events_link_across_services() {
        let backend = scan_file(
            "backend/server.py".into(),
            Lang::Python,
            "py",
            "def handler():\n    pass\napp.get(\"/api/alpha\")\n",
        );
        let frontend = scan_file(
            "frontend/api.ts".into(),
            Lang::JsTs,
            "ts",
            "export async function alpha() {\n  return fetch(\"/api/alpha\");\n}\nwindow.dispatchEvent(new CustomEvent(\"order.created\"));\n",
        );
        let worker = scan_file(
            "worker/consume.ts".into(),
            Lang::JsTs,
            "ts",
            "bus.subscribe(\"order.created\", () => {\n  run();\n});\n",
        );
        assert!(backend.provides.contains(&"route /api/alpha".to_string()), "{:?}", backend.provides);
        assert!(frontend.consumes.contains(&"route /api/alpha".to_string()), "{:?}", frontend.consumes);

        let scan = link(vec![backend, frontend, worker]);
        let has = |from: &str, to: &str, via: &str| {
            scan.edges.iter().any(|e| e.from == from && e.to == to && e.via.iter().any(|v| v == via))
        };
        assert!(
            has("frontend/api.ts", "backend/server.py", "route /api/alpha"),
            "route edge missing: {:?}",
            scan.edges
        );
        assert!(
            has("frontend/api.ts", "worker/consume.ts", "event order.created"),
            "event edge missing: {:?}",
            scan.edges
        );
    }

    /// A workspace with no git repo and no .gitignore must still never index
    /// vendored code — this is the teak-bed regression, where 2499 of 2500
    /// mapped files were inside node_modules.
    #[test]
    fn vendored_dirs_are_excluded_without_git() {
        let root = std::env::temp_dir().join(format!("sa-scan-{}", crate::db::new_id()));
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "export function vendored() {}\n").unwrap();
        std::fs::write(root.join("dist/bundle.js"), "function built() {}\n").unwrap();
        std::fs::write(root.join("src/app.ts"), "export function real() {}\n").unwrap();

        let scan = scan_workspace(&root);
        let ids: Vec<&str> = scan.files.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["src/app.ts"], "got {ids:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// End-to-end on the repository this code lives in: the scanner must map
    /// its own codebase — files found, functions found, imports resolved into
    /// edges. If a regex regresses, this is the test that notices.
    #[test]
    fn scans_this_repository() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let scan = scan_workspace(&root);
        assert!(scan.files.len() > 10, "found only {} files", scan.files.len());
        let fns: usize = scan.files.iter().map(|f| f.functions.len()).sum();
        assert!(fns > 100, "found only {fns} functions");
        assert!(!scan.edges.is_empty(), "no import edges resolved");
        // This very function must be on the map.
        let me = scan.files.iter().find(|f| f.id == "src-tauri/src/analysis/scan.rs");
        assert!(me.is_some_and(|f| f.functions.iter().any(|x| x.name == "scan_workspace")));
    }

    #[test]
    fn grades_penalise_complexity() {
        let (q, lints) = grade(200, 7, 30, "a,b,c,d,e,f,g", true);
        assert!(q < 60);
        assert!(lints.len() >= 4);
        let (perfect, none) = grade(10, 1, 2, "a", false);
        assert_eq!(perfect, 100);
        assert!(none.is_empty());
    }
}
