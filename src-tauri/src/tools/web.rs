//! Web search.
//!
//! Keyless by design: public HTML endpoints answer without an API key, so
//! search works the moment the app is installed. Results are pointers —
//! title, URL, snippet — and the tool description tells the model to follow
//! up with `fetch_url`, because a snippet is bait, not evidence. The parsers
//! are confined to this module so a paid provider can replace them later
//! without touching anything else.
//!
//! Resilience: DuckDuckGo gives the cleanest markup, but some networks cannot
//! reach it at all — connections just hang. So engines form a chain (DDG html,
//! DDG lite, Bing) with a tight per-attempt timeout, and the index of the last
//! engine that answered is remembered: a network where DDG is dead pays its
//! timeout once, not on every search.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use base64::Engine as _;
use regex_lite::Regex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A real browser string: several engines and CDNs challenge or drop unknown
/// agents. Shared with the fetch/download tools.
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Copy, PartialEq)]
enum SearchEngine {
    DdgHtml,
    DdgLite,
    Bing,
}

impl SearchEngine {
    fn name(self) -> &'static str {
        match self {
            Self::DdgHtml => "duckduckgo",
            Self::DdgLite => "duckduckgo-lite",
            Self::Bing => "bing",
        }
    }
    fn url(self, query: &str, max: usize) -> String {
        match self {
            Self::DdgHtml => format!("https://html.duckduckgo.com/html/?q={}", urlencode(query)),
            Self::DdgLite => format!("https://lite.duckduckgo.com/lite/?q={}", urlencode(query)),
            Self::Bing => format!("https://www.bing.com/search?q={}&count={}", urlencode(query), max.max(10)),
        }
    }
    fn parse(self, body: &str, max: usize) -> Vec<Hit> {
        match self {
            Self::DdgHtml => parse_html_results(body, max),
            Self::DdgLite => parse_lite_results(body, max),
            Self::Bing => parse_bing_results(body, max),
        }
    }
}

const ENGINES: &[SearchEngine] = &[SearchEngine::DdgHtml, SearchEngine::DdgLite, SearchEngine::Bing];

/// Index of the engine that answered most recently.
static PREFERRED: AtomicUsize = AtomicUsize::new(0);

pub async fn web_search(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `query`"))?
        .to_string();
    let max = args.get("max_results").and_then(Value::as_u64).unwrap_or(8).clamp(1, 10) as usize;

    let hits = search(&ctx, &query, max).await?;
    if hits.is_empty() {
        return Ok(ToolOutput::text(format!(
            "No results for \"{query}\". Try different terms, fewer quotes, or a broader phrasing."
        )));
    }

    let mut out = String::with_capacity(hits.len() * 160);
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, h.title, h.url));
        if !h.snippet.is_empty() {
            out.push_str(&format!("   {}\n", h.snippet));
        }
    }
    out.push_str("\nSnippets are teasers, not sources — fetch_url the promising ones before citing them.");

    let count = hits.len();
    Ok(ToolOutput::text(out).with_display(json!({ "kind": "search", "query": query, "count": count })))
}

/// One search, parsed: the remembered-good engine first, then the rest of the
/// chain. An engine that hangs costs its timeout once; the next search starts
/// from whichever engine actually answered.
pub async fn search(ctx: &ToolCtx, query: &str, max: usize) -> Result<Vec<Hit>> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(6))
        // Total per attempt. Without this, a network that silently drops an
        // engine's packets turned every search into a multi-minute hang.
        .timeout(Duration::from_secs(10))
        .gzip(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(BROWSER_UA)
        .build()?;

    let start = PREFERRED.load(Ordering::Relaxed).min(ENGINES.len() - 1);
    let mut errors: Vec<String> = Vec::new();
    for step in 0..ENGINES.len() {
        let idx = (start + step) % ENGINES.len();
        let engine = ENGINES[idx];
        match get_text(ctx, &client, &engine.url(query, max)).await {
            Ok(body) => {
                let hits = engine.parse(&body, max);
                if hits.is_empty() {
                    errors.push(format!("{}: answered but no results parsed", engine.name()));
                } else {
                    PREFERRED.store(idx, Ordering::Relaxed);
                    return Ok(hits);
                }
            }
            Err(AppError::Cancelled) => return Err(AppError::Cancelled),
            Err(e) => errors.push(format!("{}: {e}", engine.name())),
        }
    }
    Err(AppError::msg(format!(
        "every search engine failed — {}. If this network blocks search endpoints, go direct: fetch_url a site you already know (docs, GitHub, Wikipedia).",
        errors.join("; ")
    )))
}

async fn get_text(ctx: &ToolCtx, client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
        r = client.get(url).header("Accept-Language", "en").send() => r?,
    };
    if !resp.status().is_success() {
        return Err(AppError::msg(format!("search endpoint answered {}", resp.status())));
    }
    Ok(resp.text().await?)
}

/// html.duckduckgo.com: results are `result__a` anchors wrapping a redirect
/// URL whose `uddg` parameter is the real destination.
fn parse_html_results(body: &str, max: usize) -> Vec<Hit> {
    let re_link =
        Regex::new(r#"<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).expect("static regex");
    let re_snip = Regex::new(r#"class="result__snippet"[^>]*>(.*?)</a>"#).expect("static regex");
    let snippets: Vec<String> = re_snip.captures_iter(body).map(|c| clean(&c[1])).collect();

    let mut hits = Vec::new();
    for (i, c) in re_link.captures_iter(body).enumerate() {
        let Some(url) = unwrap_redirect(&c[1]) else { continue };
        hits.push(Hit { title: clean(&c[2]), url, snippet: snippets.get(i).cloned().unwrap_or_default() });
        if hits.len() >= max {
            break;
        }
    }
    hits
}

/// lite.duckduckgo.com: a plain table of `<a rel="nofollow" href="…">` rows.
fn parse_lite_results(body: &str, max: usize) -> Vec<Hit> {
    let re = Regex::new(r#"<a[^>]*rel="nofollow"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).expect("static regex");
    let mut hits = Vec::new();
    for c in re.captures_iter(body) {
        let raw = &c[1];
        let Some(url) = unwrap_redirect(raw) else { continue };
        if !url.starts_with("http") {
            continue;
        }
        hits.push(Hit { title: clean(&c[2]), url, snippet: String::new() });
        if hits.len() >= max {
            break;
        }
    }
    hits
}

/// bing.com/search: each organic result is a `<li class="b_algo">` block with
/// an `<h2><a href>` title and a `<p>` snippet. Links usually hide behind a
/// `bing.com/ck/a?…&u=a1<base64url>` tracking hop, unwrapped here.
fn parse_bing_results(body: &str, max: usize) -> Vec<Hit> {
    let re_a = Regex::new(r#"<h2[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).expect("static regex");
    let re_p = Regex::new(r#"<p[^>]*>(.*?)</p>"#).expect("static regex");

    let mut hits: Vec<Hit> = Vec::new();
    let mut cursor = 0;
    while let Some(at) = body[cursor..].find("<li class=\"b_algo\"") {
        let start = cursor + at;
        let end = body[start + 1..]
            .find("<li class=\"b_algo\"")
            .map(|n| start + 1 + n)
            .unwrap_or(body.len());
        let block = &body[start..end];
        cursor = end;

        let Some(c) = re_a.captures(block) else { continue };
        let Some(url) = unwrap_bing(&c[1]) else { continue };
        if hits.iter().any(|h| h.url == url) {
            continue;
        }
        let snippet = re_p.captures(block).map(|p| clean(&p[1])).unwrap_or_default();
        hits.push(Hit { title: clean(&c[2]), url, snippet });
        if hits.len() >= max {
            break;
        }
    }
    hits
}

/// href attributes come out of the markup HTML-escaped — `&amp;` between
/// query params. Parse the value un-escaped or every parameter after the
/// first is named `amp;something` and lookups silently miss.
fn unescape_href(href: &str) -> String {
    href.replace("&amp;", "&").replace("&#38;", "&").replace("&#x26;", "&").replace("&quot;", "\"")
}

/// Bing tracking hops carry the destination as `u=a1<base64url>`.
fn unwrap_bing(href: &str) -> Option<String> {
    let href = unescape_href(href);
    let absolute = if href.starts_with("//") { format!("https:{href}") } else { href };
    if absolute.contains("bing.com/ck/") {
        let parsed = reqwest::Url::parse(&absolute).ok()?;
        let u = parsed.query_pairs().find(|(k, _)| k == "u").map(|(_, v)| v.into_owned())?;
        let encoded = u.strip_prefix("a1").unwrap_or(&u).trim_end_matches('=');
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()?;
        let url = String::from_utf8(bytes).ok()?;
        return url.starts_with("http").then_some(url);
    }
    (absolute.starts_with("http") && !absolute.contains("bing.com/search")).then_some(absolute)
}

/// DDG links point at `/l/?uddg=<encoded real url>`; direct links pass through.
fn unwrap_redirect(href: &str) -> Option<String> {
    let href = unescape_href(href);
    let absolute = if href.starts_with("//") { format!("https:{href}") } else { href };
    if absolute.contains("duckduckgo.com/l/") {
        let parsed = reqwest::Url::parse(&absolute).ok()?;
        return parsed.query_pairs().find(|(k, _)| k == "uddg").map(|(_, v)| v.into_owned());
    }
    if absolute.starts_with("http") {
        return Some(absolute);
    }
    None
}

fn clean(fragment: &str) -> String {
    let re_tag = Regex::new(r"<[^>]+>").expect("static regex");
    let stripped = re_tag.replace_all(fragment, "");
    stripped
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
// A panic in a test is a failed test, which is exactly what we want here.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_ddg_redirects() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs%3Fq%3D1&rut=abc";
        assert_eq!(unwrap_redirect(href).as_deref(), Some("https://example.com/docs?q=1"));
        assert_eq!(
            unwrap_redirect("https://direct.example.com").as_deref(),
            Some("https://direct.example.com")
        );
        assert_eq!(unwrap_redirect("javascript:void(0)"), None);
    }

    #[test]
    fn parses_bing_blocks_and_unwraps_tracking() {
        // Exactly the served shape: one minified line, `<h2 class="">`, and an
        // HTML-ESCAPED tracking href (`&amp;` separators) — the escaping is
        // what silently zeroed every real-world parse until it was unescaped.
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("https://example.com/x");
        let body = format!(
            r#"<ol id="b_results"><li class="b_algo" data-id iid=SERP.1><h2 class=""><a target="_blank" href="https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1{b64}&amp;ntb=1">Tracked <b>Hit</b></a></h2><div class="b_caption"><p class="b_lineclamp2">A &amp; snippet</p></div></li><li class="b_algo"><h2><a href="https://direct.dev/page">Direct</a></h2></li></ol>"#
        );
        let hits = parse_bing_results(&body, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/x");
        assert_eq!(hits[0].title, "Tracked Hit");
        assert_eq!(hits[0].snippet, "A & snippet");
        assert_eq!(hits[1].url, "https://direct.dev/page");
    }

    /// Live probe — run by hand when search misbehaves on a new network:
    /// `cargo test live_bing_parses -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "hits the live network"]
    async fn live_bing_parses() {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(6))
            .timeout(Duration::from_secs(10))
            .gzip(true)
            .user_agent(BROWSER_UA)
            .build()
            .unwrap();
        let body = client
            .get(SearchEngine::Bing.url("rust tokio tutorial", 8))
            .header("Accept-Language", "en")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let hits = parse_bing_results(&body, 8);
        for h in &hits {
            println!("{} — {}", h.url, h.title);
        }
        assert!(hits.len() >= 3, "only {} hits parsed from live bing ({} bytes)", hits.len(), body.len());
    }

    #[test]
    fn parses_html_result_markup() {
        let body = r#"
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.dev%2F">First <b>Hit</b></a>
            <a class="result__snippet" href="x">Some &amp; useful snippet</a>
            <a rel="nofollow" class="result__a" href="https://b.dev/page">Second</a>
        "#;
        let hits = parse_html_results(body, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "First Hit");
        assert_eq!(hits[0].url, "https://a.dev/");
        assert_eq!(hits[0].snippet, "Some & useful snippet");
        assert_eq!(hits[1].url, "https://b.dev/page");
    }
}
