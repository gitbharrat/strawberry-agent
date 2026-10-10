//! The Model Execution Graph executor.
//!
//! A graph is a Fusion-style flow of stages between the user's message and the
//! final answer: research feeds planning, planning feeds execution, and
//! verification loops back until the work passes. Each node carries its own
//! model, reasoning effort and standing instruction; edges carry artifacts.
//!
//! Design notes:
//!  * A graph run is a first-class `Run` — same registry, same cancellation,
//!    same `run_subscribe` reattachment, same cost accounting as a chat run.
//!    Graph-specific events ride the existing `AgentEvent` broadcast.
//!  * Scheduling is forward-topological with bounded parallelism; parallel
//!    branches genuinely run together.
//!  * Loops are explicit backward edges with an iteration budget. A verdict
//!    node deciding "fail" re-arms everything downstream of the loop target
//!    (the dirty cone), injects its issues as feedback, and tries again.
//!  * Node outputs are content-hash cached ComfyUI-style and persisted with
//!    the graph, so an unchanged research brief or colour theme is never paid
//!    for twice. Pinned nodes always serve their cache.

use crate::agent::runner::RunContext;
use crate::agent::AgentEvent;
use crate::db::NewMessage;
use crate::error::{AppError, Result};
use crate::openrouter::{ChatRequest, StreamEvent, ToolCall, Usage};
use crate::settings::{PermissionMode, Settings};
use crate::state::AppState;
use crate::tools::{self, ToolEnv};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

// ------------------------------------------------------------------ wire types

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeConfig {
    /// Tool reach for agent-backed nodes: none | read | web | edit | full.
    pub tools: Option<String>,
    pub fanout: Option<u32>,
    pub include_goal: Option<bool>,
    /// Verdict nodes: continue | fail when the loop budget runs dry.
    pub on_exhaust: Option<String>,
    /// auto (re-run when inputs change) | always (fresh every time) |
    /// once (at most once per run — a loop re-pass keeps serving its result).
    pub rerun: Option<String>,
    /// Include the last N chat messages as context for this node.
    pub history: Option<u32>,
    /// Agent nodes may search past conversations (memory tools).
    pub memory: Option<bool>,
}

/// A visual grouping of nodes — Fusion's underlay/group. Execution ignores
/// it entirely; a collapsed group just renders as one tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphGroup {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub pinned: bool,
    /// Membership in a visual group, if any.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub config: NodeConfig,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub id: String,
    pub from: String,
    #[serde(default = "out_port")]
    pub from_port: String,
    pub to: String,
    #[serde(rename = "loop", default)]
    pub is_loop: bool,
    #[serde(default = "default_iters")]
    pub max_iters: u32,
}

fn out_port() -> String {
    "out".into()
}
fn default_iters() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSpec {
    pub id: String,
    pub name: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub groups: Vec<GraphGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeArtifact {
    pub hash: String,
    pub content: String,
    pub verdict: Option<String>,
    pub model: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost: f64,
    pub elapsed_ms: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Verdict {
    pub pass: bool,
    pub score: Option<f64>,
    pub issues: Vec<String>,
    pub summary: String,
}

/// A stored graph as the frontend sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphDoc {
    pub id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub spec: GraphSpec,
    pub artifacts: HashMap<String, NodeArtifact>,
    pub updated_at: i64,
}

// ------------------------------------------------------------------- executor

/// Hard ceiling on node executions per run — the recursion-limit backstop.
const MAX_EXECUTIONS: u32 = 64;
/// Concurrent node executions. Parallel branches run together, but a graph
/// never floods the provider.
const PARALLEL: usize = 3;

/// Everything a single node execution needs, cloneable into its task.
struct NodeJob {
    state: Arc<AppState>,
    handle: Arc<crate::agent::RunHandle>,
    run_id: String,
    chat_id: String,
    settings: Settings,
    cwd: PathBuf,
    remote: Option<Arc<crate::remote::Remote>>,
    allowed_roots: Vec<PathBuf>,
    denied_paths: Vec<PathBuf>,
    node: GraphNode,
    model: String,
    context: String,
    goal: String,
    attempt: u32,
    hash: String,
    /// Verdict nodes ask for the machine-readable block; others do not.
    wants_verdict: bool,
}

struct NodeOutcome {
    artifact: NodeArtifact,
    verdict: Option<Verdict>,
}

#[derive(Clone, Copy, PartialEq)]
enum NState {
    Pending,
    Running,
    Done,
    Cached,
    Skipped,
}

#[derive(Clone, Copy, PartialEq)]
enum EState {
    Unresolved,
    Active,
    Inactive,
}

struct RunState {
    nodes: HashMap<String, NState>,
    /// Artifacts produced or served this run, by node id.
    artifacts: HashMap<String, NodeArtifact>,
    verdicts: HashMap<String, Verdict>,
    edges: HashMap<String, EState>,
    feedback: HashMap<String, Vec<String>>,
    attempts: HashMap<String, u32>,
    loop_used: HashMap<String, u32>,
    executions: u32,
}

pub struct GraphRun {
    pub ctx: RunContext,
    pub graph_id: String,
}

/// Entry point, `tokio::spawn`ed. Terminates exactly like a chat run: emits
/// `RunFinished`, finishes the DB row, clears the registry entry.
pub async fn execute(run: GraphRun) {
    let ctx = &run.ctx;
    let outcome = run_inner(&run).await;

    if !ctx.handle.cancel.is_cancelled() {
        if let Ok(key) = ctx.state.api_key() {
            let client = ctx.state.client();
            let model = ctx.state.settings().default_model;
            crate::agent::runner::maybe_title(ctx, &client, &key, &model).await;
        }
    }

    let (status, error) = match &outcome {
        Ok(()) => ("complete", None),
        Err(AppError::Cancelled) => ("cancelled", None),
        Err(e) => ("error", Some(e.to_string())),
    };
    if status == "complete" {
        crate::analysis::maybe_auto_update(ctx.state.clone(), ctx.chat_id.clone(), ctx.run_id.clone());
    }
    let _ = ctx.state.db.finish_run(&ctx.run_id, status, error.as_deref());
    let _ = ctx.state.db.touch_chat(&ctx.chat_id);
    ctx.emit(AgentEvent::RunFinished { run_id: ctx.run_id.clone(), status: status.into(), error });
    ctx.state.runs.finish(&ctx.run_id);
}

async fn run_inner(run: &GraphRun) -> Result<()> {
    let ctx = &run.ctx;
    let state = &ctx.state;
    let settings = state.settings();
    state.api_key()?;

    let row = state.db.get_graph(&run.graph_id)?;
    let spec: GraphSpec = serde_json::from_str(&row.spec)
        .map_err(|e| AppError::msg(format!("this graph cannot be read: {e}")))?;
    let stored: HashMap<String, NodeArtifact> = serde_json::from_str(&row.artifacts).unwrap_or_default();

    let chat = state.db.get_chat(&ctx.chat_id)?;
    let project = chat.project_id.as_deref().and_then(|id| state.db.get_project(id).ok());

    // The goal is the latest user message — what this run is answering. The
    // rest of the transcript is kept for nodes that opt into history.
    let chat_history = state.db.list_messages(&ctx.chat_id)?;
    let goal = chat_history
        .iter()
        .rev()
        .find(|m| m.role == "user" && !m.content.trim().is_empty())
        .map(|m| m.content.clone())
        .unwrap_or_default();

    // Workspace resolution, identical to a chat run so tools behave the same.
    let remote_target = chat.cwd.as_deref().and_then(crate::remote::parse);
    let (cwd, remote) = match &remote_target {
        Some(t) => {
            let r = crate::remote::pooled(state, &t.host_id)
                .await
                .map_err(|e| AppError::msg(format!("the remote workspace is unreachable: {e}")))?;
            (PathBuf::from(&t.root), Some(r))
        }
        None => (crate::agent::runner::resolve_cwd(&chat, project.as_ref()), None),
    };
    let allowed_roots = crate::agent::runner::resolve_roots(&settings, &cwd);
    let denied_paths: Vec<PathBuf> =
        settings.denied_paths.iter().map(|p| crate::tools::expand_home(p)).collect();

    let default_model = chat.model.clone().unwrap_or_else(|| settings.default_model.clone());

    ctx.emit(AgentEvent::RunStarted {
        run_id: ctx.run_id.clone(),
        chat_id: ctx.chat_id.clone(),
        model: format!("graph:{}", spec.name),
    });

    // ---- structural validation
    // A note wired into a stage is context, not decoration: it runs (for free)
    // and its text flows down the wire. Unwired notes stay decorative.
    let wired: HashSet<&str> =
        spec.edges.iter().filter(|e| !e.is_loop).map(|e| e.from.as_str()).collect();
    let nodes: Vec<&GraphNode> = spec
        .nodes
        .iter()
        .filter(|n| n.kind != "note" || wired.contains(n.id.as_str()))
        .collect();
    if nodes.iter().all(|n| n.kind == "note") {
        return Err(AppError::msg("the graph has no nodes"));
    }
    let roster: Vec<Value> = nodes
        .iter()
        .filter(|n| n.kind != "note")
        .map(|n| json!({ "id": n.id, "label": n.label, "kind": n.kind }))
        .collect();
    ctx.emit(AgentEvent::GraphStarted {
        graph_id: run.graph_id.clone(),
        name: spec.name.clone(),
        nodes: Value::Array(roster),
    });
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let forward: Vec<&GraphEdge> = spec
        .edges
        .iter()
        .filter(|e| !e.is_loop && ids.contains(e.from.as_str()) && ids.contains(e.to.as_str()))
        .collect();
    let loops: Vec<&GraphEdge> = spec
        .edges
        .iter()
        .filter(|e| e.is_loop && ids.contains(e.from.as_str()) && ids.contains(e.to.as_str()))
        .collect();
    if topo_order(&nodes, &forward).is_none() {
        return Err(AppError::msg(
            "the graph's forward flow runs in a circle — mark backward arrows as loops",
        ));
    }

    let mut rs = RunState {
        nodes: nodes.iter().map(|n| (n.id.clone(), NState::Pending)).collect(),
        artifacts: HashMap::new(),
        verdicts: HashMap::new(),
        edges: forward.iter().map(|e| (e.id.clone(), EState::Unresolved)).collect(),
        feedback: HashMap::new(),
        attempts: HashMap::new(),
        loop_used: HashMap::new(),
        executions: 0,
    };
    for n in &nodes {
        ctx.emit(AgentEvent::GraphNode { node_id: n.id.clone(), status: "pending".into(), detail: None });
    }

    let by_id: HashMap<&str, &GraphNode> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
    let mut running: tokio::task::JoinSet<(String, Result<NodeOutcome>)> = tokio::task::JoinSet::new();
    let mut in_flight: HashSet<String> = HashSet::new();
    let mut step: u32 = 0;

    loop {
        if ctx.handle.cancel.is_cancelled() {
            running.abort_all();
            return Err(AppError::Cancelled);
        }

        // Launch every ready node, up to the parallelism cap.
        while in_flight.len() < PARALLEL {
            let Some(node) = next_ready(&nodes, &forward, &rs, &in_flight) else { break };
            let nid = node.id.clone();

            // A node whose every input branch was ruled out never runs.
            if fully_inactive(node, &forward, &rs) {
                set_state(ctx, &mut rs, &nid, NState::Skipped, Some("branch not taken"));
                resolve_out_edges(&mut rs, &forward, &nid, EState::Inactive, None);
                continue;
            }

            let mut context = assemble_context(node, &spec, &forward, &rs, &by_id);
            if let Some(h) = node.config.history.filter(|h| *h > 0) {
                context.push_str(&history_section(&chat_history, h as usize));
            }
            let context = context;
            let attempt = *rs.attempts.get(&nid).unwrap_or(&0);
            let feedback = rs.feedback.get(&nid).cloned().unwrap_or_default();
            let context = if feedback.is_empty() {
                context
            } else {
                format!("{context}\n## Feedback from the last attempt — fix these first\n{}\n", feedback.join("\n"))
            };

            // Bypassed nodes and structural nodes are free: no model call.
            if !node.enabled {
                let art = free_artifact(&context, "bypassed");
                rs.artifacts.insert(nid.clone(), art);
                set_state(ctx, &mut rs, &nid, NState::Skipped, Some("bypassed"));
                resolve_out_edges(&mut rs, &forward, &nid, EState::Active, None);
                continue;
            }
            if let Some(content) = builtin_content(node, &goal, &context, &cwd) {
                let art = free_artifact(&content, &node.kind);
                emit_done(ctx, &nid, "done", &art, None);
                rs.artifacts.insert(nid.clone(), art);
                set_state(ctx, &mut rs, &nid, NState::Done, None);
                resolve_out_edges(&mut rs, &forward, &nid, EState::Active, None);
                continue;
            }

            let model = node.model.clone().unwrap_or_else(|| default_model.clone());
            let include_goal = node.config.include_goal.unwrap_or(true);
            let hash = content_hash(node, &model, &context, if include_goal { &goal } else { "" });

            // The cache: pinned nodes always serve their stored output; pure
            // nodes serve it when nothing upstream changed. Workspace-touching
            // agents are never auto-cached — the disk is part of their input.
            if let Some(art) = stored.get(&nid) {
                let auto = cacheable(node) && art.hash == hash && attempt == 0;
                if node.pinned || auto {
                    let mut art = art.clone();
                    art.hash = hash.clone();
                    let verdict: Option<Verdict> =
                        art.verdict.as_deref().and_then(|v| serde_json::from_str(v).ok());
                    if node.kind == "output" {
                        deliver_to_chat(ctx, &art.content, &model)?;
                    }
                    emit_done(ctx, &nid, "cached", &art, verdict.as_ref());
                    if let Some(v) = &verdict {
                        rs.verdicts.insert(nid.clone(), v.clone());
                    }
                    rs.artifacts.insert(nid.clone(), art);
                    set_state(ctx, &mut rs, &nid, NState::Cached, Some(if node.pinned { "pinned" } else { "unchanged" }));
                    route_after(ctx, &mut rs, node, &forward, &loops, &by_id);
                    continue;
                }
            }

            rs.executions += 1;
            if rs.executions > MAX_EXECUTIONS {
                return Err(AppError::msg(format!(
                    "stopped after {MAX_EXECUTIONS} node executions — a loop is not converging"
                )));
            }
            step += 1;
            ctx.emit(AgentEvent::StepStarted { step });
            set_state(ctx, &mut rs, &nid, NState::Running, None);
            in_flight.insert(nid.clone());

            let job = NodeJob {
                state: state.clone(),
                handle: ctx.handle.clone(),
                run_id: ctx.run_id.clone(),
                chat_id: ctx.chat_id.clone(),
                settings: settings.clone(),
                cwd: cwd.clone(),
                remote: remote.clone(),
                allowed_roots: allowed_roots.clone(),
                denied_paths: denied_paths.clone(),
                node: node.clone(),
                model,
                context,
                goal: if include_goal { goal.clone() } else { String::new() },
                attempt,
                hash,
                wants_verdict: is_verdict(node),
            };
            running.spawn(async move {
                let id = job.node.id.clone();
                (id, execute_node(job).await)
            });
        }

        if in_flight.is_empty() {
            break; // Nothing running and nothing ready: the graph is finished.
        }

        let Some(joined) = running.join_next().await else { break };
        let (nid, result) = joined.map_err(|e| AppError::msg(format!("a node task panicked: {e}")))?;
        in_flight.remove(&nid);
        let node = by_id[nid.as_str()];

        match result {
            Ok(out) => {
                // Persist the artifact write-through, so the cache survives
                // anything that happens after it.
                let mut all = stored.clone();
                for (k, v) in &rs.artifacts {
                    all.insert(k.clone(), v.clone());
                }
                all.insert(nid.clone(), out.artifact.clone());
                if let Ok(json) = serde_json::to_string(&all) {
                    let _ = state.db.set_graph_artifacts(&run.graph_id, &json);
                }

                emit_done(ctx, &nid, "done", &out.artifact, out.verdict.as_ref());
                rs.artifacts.insert(nid.clone(), out.artifact);
                if let Some(v) = out.verdict {
                    rs.verdicts.insert(nid.clone(), v);
                }
                set_state(ctx, &mut rs, &nid, NState::Done, None);
                route_after(ctx, &mut rs, node, &forward, &loops, &by_id);
            }
            Err(AppError::Cancelled) => {
                running.abort_all();
                return Err(AppError::Cancelled);
            }
            Err(e) => {
                ctx.emit(AgentEvent::GraphNode {
                    node_id: nid.clone(),
                    status: "failed".into(),
                    detail: Some(e.to_string()),
                });
                running.abort_all();
                return Err(AppError::msg(format!("{} failed: {e}", node.label)));
            }
        }
    }

    // No Output node ran? The terminal artifacts still owe the user an answer.
    let has_output = nodes
        .iter()
        .any(|n| n.kind == "output" && matches!(rs.nodes.get(&n.id), Some(NState::Done | NState::Cached)));
    if !has_output {
        let mut parts: Vec<String> = Vec::new();
        for n in &nodes {
            let terminal = !forward.iter().any(|e| e.from == n.id);
            if terminal {
                if let Some(a) = rs.artifacts.get(&n.id) {
                    if !a.content.trim().is_empty() {
                        parts.push(format!("## {}\n\n{}", n.label, a.content.trim()));
                    }
                }
            }
        }
        let text = if parts.is_empty() {
            "The graph finished without producing output.".to_string()
        } else {
            parts.join("\n\n")
        };
        deliver_to_chat(ctx, &text, &default_model)?;
    }
    Ok(())
}

// ----------------------------------------------------------------- scheduling

fn topo_order<'a>(nodes: &[&'a GraphNode], forward: &[&'a GraphEdge]) -> Option<Vec<&'a str>> {
    let mut indeg: HashMap<&str, usize> = nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    for e in forward {
        *indeg.get_mut(e.to.as_str())? += 1;
    }
    let mut queue: Vec<&str> =
        nodes.iter().map(|n| n.id.as_str()).filter(|id| indeg[id] == 0).collect();
    let mut out = Vec::new();
    while let Some(id) = queue.pop() {
        out.push(id);
        for e in forward.iter().filter(|e| e.from == id) {
            let Some(d) = indeg.get_mut(e.to.as_str()) else { continue };
            *d -= 1;
            if *d == 0 {
                queue.push(e.to.as_str());
            }
        }
    }
    (out.len() == nodes.len()).then_some(out)
}

/// The next pending node whose every incoming forward edge is resolved.
fn next_ready<'a>(
    nodes: &[&'a GraphNode],
    forward: &[&GraphEdge],
    rs: &RunState,
    in_flight: &HashSet<String>,
) -> Option<&'a GraphNode> {
    nodes.iter().copied().find(|n| {
        rs.nodes.get(&n.id) == Some(&NState::Pending)
            && !in_flight.contains(&n.id)
            && forward
                .iter()
                .filter(|e| e.to == n.id)
                .all(|e| rs.edges.get(&e.id) != Some(&EState::Unresolved))
    })
}

fn fully_inactive(node: &GraphNode, forward: &[&GraphEdge], rs: &RunState) -> bool {
    let incoming: Vec<_> = forward.iter().filter(|e| e.to == node.id).collect();
    !incoming.is_empty() && incoming.iter().all(|e| rs.edges.get(&e.id) == Some(&EState::Inactive))
}

fn set_state(ctx: &RunContext, rs: &mut RunState, nid: &str, st: NState, detail: Option<&str>) {
    rs.nodes.insert(nid.to_string(), st);
    let status = match st {
        NState::Pending => "pending",
        NState::Running => "running",
        NState::Done => "done",
        NState::Cached => "cached",
        NState::Skipped => "skipped",
    };
    ctx.emit(AgentEvent::GraphNode {
        node_id: nid.to_string(),
        status: status.into(),
        detail: detail.map(str::to_string),
    });
}

/// Marks this node's out-edges. `only_port` restricts to one port (verdict
/// routing); the rest go inactive.
fn resolve_out_edges(
    rs: &mut RunState,
    forward: &[&GraphEdge],
    nid: &str,
    st: EState,
    only_port: Option<&str>,
) {
    for e in forward.iter().filter(|e| e.from == nid) {
        let state = match only_port {
            Some(port) if e.from_port != port => EState::Inactive,
            _ => st,
        };
        rs.edges.insert(e.id.clone(), state);
    }
}

/// Post-completion routing: plain nodes light all out-edges; verdict nodes
/// pick a port, and a failing verdict with loop budget left re-arms the loop
/// body instead of moving on.
fn route_after(
    ctx: &RunContext,
    rs: &mut RunState,
    node: &GraphNode,
    forward: &[&GraphEdge],
    loops: &[&GraphEdge],
    by_id: &HashMap<&str, &GraphNode>,
) {
    if !is_verdict(node) {
        resolve_out_edges(rs, forward, &node.id, EState::Active, None);
        return;
    }
    let pass = rs.verdicts.get(&node.id).map(|v| v.pass).unwrap_or(true);
    let (ok_port, bad_port) =
        if node.kind == "gate" { ("approve", "reject") } else { ("pass", "fail") };

    if pass {
        resolve_out_edges(rs, forward, &node.id, EState::Active, Some(ok_port));
        return;
    }

    // Failing: is there a loop with budget left?
    if let Some(edge) = loops.iter().find(|e| e.from == node.id) {
        let used = rs.loop_used.entry(edge.id.clone()).or_insert(0);
        if *used < edge.max_iters {
            *used += 1;
            let iteration = *used;
            ctx.emit(AgentEvent::GraphLoop {
                edge_id: edge.id.clone(),
                iteration,
                max_iters: edge.max_iters,
            });
            let issues = rs
                .verdicts
                .get(&node.id)
                .map(|v| {
                    let mut fb = v.issues.clone();
                    if fb.is_empty() && !v.summary.is_empty() {
                        fb.push(v.summary.clone());
                    }
                    fb
                })
                .unwrap_or_default();
            rearm_cone(ctx, rs, &edge.to, forward, by_id);
            rs.feedback.entry(edge.to.clone()).or_default().extend(issues);
            *rs.attempts.entry(edge.to.clone()).or_insert(0) += 1;
            return;
        }
        // Budget exhausted.
        let on_exhaust = node.config.on_exhaust.as_deref().unwrap_or("continue");
        if on_exhaust == "continue" {
            ctx.emit(AgentEvent::Notice {
                level: "warn".into(),
                text: format!(
                    "{}: loop budget exhausted after {} attempt(s) — continuing with the work as it stands.",
                    node.label, edge.max_iters
                ),
            });
            resolve_out_edges(rs, forward, &node.id, EState::Active, Some(ok_port));
            return;
        }
    }
    // A final fail routes the fail branch, if one exists.
    resolve_out_edges(rs, forward, &node.id, EState::Active, Some(bad_port));
}

/// Resets the loop target and every forward descendant that already ran —
/// the dirty cone — so the retry flows through cleanly.
fn rearm_cone(
    ctx: &RunContext,
    rs: &mut RunState,
    target: &str,
    forward: &[&GraphEdge],
    by_id: &HashMap<&str, &GraphNode>,
) {
    let mut cone: HashSet<String> = HashSet::new();
    let mut stack = vec![target.to_string()];
    while let Some(cur) = stack.pop() {
        if !cone.insert(cur.clone()) {
            continue;
        }
        for e in forward.iter().filter(|e| e.from == cur) {
            stack.push(e.to.clone());
        }
    }
    // "Once" nodes that already ran keep serving their result through loop
    // passes — the loop target itself always re-runs, or the loop is a no-op.
    let keep: HashSet<&String> = cone
        .iter()
        .filter(|id| id.as_str() != target)
        .filter(|id| {
            by_id.get(id.as_str()).is_some_and(|n| n.config.rerun.as_deref() == Some("once"))
                && matches!(rs.nodes.get(id.as_str()), Some(NState::Done | NState::Cached))
        })
        .collect();
    let keep: HashSet<String> = keep.into_iter().cloned().collect();
    for nid in &cone {
        if keep.contains(nid) {
            continue;
        }
        if by_id.contains_key(nid.as_str()) {
            rs.artifacts.remove(nid);
            rs.verdicts.remove(nid);
            set_state(ctx, rs, nid, NState::Pending, Some("loop"));
        }
    }
    for e in forward {
        // Every edge leaving a re-armed node is unresolved again; edges from
        // kept nodes and from outside the cone keep their settled inputs.
        if cone.contains(&e.from) && !keep.contains(&e.from) {
            rs.edges.insert(e.id.clone(), EState::Unresolved);
        }
    }
}

// ----------------------------------------------------------- context assembly

/// Everything flowing into a node over its active edges, as labeled sections.
fn assemble_context(
    node: &GraphNode,
    spec: &GraphSpec,
    forward: &[&GraphEdge],
    rs: &RunState,
    by_id: &HashMap<&str, &GraphNode>,
) -> String {
    let mut sections = String::new();
    for e in spec.edges.iter().filter(|e| !e.is_loop && e.to == node.id) {
        if forward.iter().all(|f| f.id != e.id) {
            continue;
        }
        if rs.edges.get(&e.id) != Some(&EState::Active) {
            continue;
        }
        let Some(art) = rs.artifacts.get(&e.from) else { continue };
        if art.content.trim().is_empty() {
            continue;
        }
        let label = by_id.get(e.from.as_str()).map(|n| n.label.as_str()).unwrap_or("input");
        sections.push_str(&format!("## Input — {label}\n\n{}\n\n", art.content.trim()));
    }
    sections
}

/// The conversation so far, compactly, for nodes that opt into history. The
/// trailing user message is excluded — that is the goal, already present.
fn history_section(messages: &[crate::db::Message], n: usize) -> String {
    let mut list: Vec<&crate::db::Message> = messages
        .iter()
        .filter(|m| (m.role == "user" || m.role == "assistant") && !m.content.trim().is_empty())
        .collect();
    if list.last().is_some_and(|m| m.role == "user") {
        list.pop();
    }
    if list.is_empty() {
        return String::new();
    }
    let tail = &list[list.len().saturating_sub(n)..];
    let mut out = String::from("## Conversation so far\n\n");
    for m in tail {
        let clipped: String = m.content.chars().take(700).collect();
        let ellipsis = if m.content.chars().count() > 700 { "…" } else { "" };
        out.push_str(&format!("{}: {clipped}{ellipsis}\n\n", m.role));
    }
    out
}

/// Structural nodes produce their output without a model call: merges join,
/// the arch-map node reads the persisted architecture document, empty-prompt
/// thinking nodes pass through. Returns None when a real model call is needed.
fn builtin_content(node: &GraphNode, goal: &str, context: &str, cwd: &std::path::Path) -> Option<String> {
    match node.kind.as_str() {
        // A wired note IS its text: reference criteria, checklists, standing
        // context — handed to downstream stages verbatim, for free.
        "note" => Some(node.prompt.trim().to_string()),
        "merge" => Some(if context.trim().is_empty() { goal.to_string() } else { context.trim().to_string() }),
        "archmap" => Some(match crate::analysis::prompt_context(cwd) {
            Some(map) => map,
            None => "No architecture map exists for this workspace yet. Open the Architecture pane and run a scan to give this node something to emit.".to_string(),
        }),
        "goal" if node.prompt.trim().is_empty() => Some(goal.to_string()),
        // These run real machinery even with an empty prompt — never a wire.
        "mapupdate" | "sketch" => None,
        _ if node.prompt.trim().is_empty() && !is_agentic(node) && node.kind != "gate" => {
            // An instruction-less thinking node is a wire, not a call.
            Some(if context.trim().is_empty() { goal.to_string() } else { context.trim().to_string() })
        }
        _ => None,
    }
}

fn free_artifact(content: &str, _why: &str) -> NodeArtifact {
    NodeArtifact {
        hash: String::new(),
        content: content.to_string(),
        verdict: None,
        model: None,
        tokens_in: 0,
        tokens_out: 0,
        cost: 0.0,
        elapsed_ms: 0,
        created_at: chrono::Utc::now().timestamp_millis(),
    }
}

fn is_verdict(node: &GraphNode) -> bool {
    matches!(node.kind.as_str(), "verify" | "test" | "gate")
}

fn is_agentic(node: &GraphNode) -> bool {
    matches!(
        node.kind.as_str(),
        "research"
            | "fetch"
            | "codebase"
            | "environment"
            | "execute"
            | "test"
            | "verify"
            | "optimize"
            | "refactor"
            | "documentation"
    )
}

fn cacheable(node: &GraphNode) -> bool {
    // An "always" node is fresh every time by request.
    if node.config.rerun.as_deref() == Some("always") {
        return false;
    }
    // Workspace-touching stages read the disk, which the hash cannot see.
    if node.kind == "mapupdate" {
        return false;
    }
    let tools = node.config.tools.as_deref().unwrap_or(default_tools(&node.kind));
    !is_agentic(node) || matches!(tools, "none" | "web")
}

fn default_tools(kind: &str) -> &'static str {
    match kind {
        "research" => "web",
        "codebase" | "environment" => "read",
        // Verification is exercise, not critique: the verifier builds, runs,
        // screenshots and harnesses — so it needs the full kit by default.
        // Fetch downloads into the workspace, so it needs the kit too.
        "execute" | "test" | "verify" | "fetch" => "full",
        "optimize" | "refactor" | "documentation" => "edit",
        _ => "none",
    }
}

/// FNV-1a over everything that shapes a node's output. Matching hash → the
/// cached artifact is still true, and the tokens need not be spent again.
fn content_hash(node: &GraphNode, model: &str, context: &str, goal: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |s: &str| {
        for b in s.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0x1f;
        h = h.wrapping_mul(0x100000001b3);
    };
    eat(&node.kind);
    eat(&node.prompt);
    eat(model);
    eat(node.effort.as_deref().unwrap_or(""));
    eat(node.config.tools.as_deref().unwrap_or(""));
    eat(node.config.rerun.as_deref().unwrap_or(""));
    if node.config.memory == Some(true) {
        eat("mem");
    }
    eat(context);
    eat(goal);
    format!("{h:016x}")
}

// ------------------------------------------------------------- node execution

async fn execute_node(job: NodeJob) -> Result<NodeOutcome> {
    let started = Instant::now();
    if job.node.kind == "gate" {
        return gate_node(&job).await;
    }
    if job.node.kind == "sketch" {
        return sketch_node(&job).await;
    }
    if job.node.kind == "mapupdate" {
        return map_update_node(&job, started).await;
    }

    let user = build_user_message(&job);
    let system = build_system(&job);

    let (content, tokens, verdict) = if job.node.kind == "design" {
        design_node(&job).await?
    } else if is_agentic(&job.node) {
        run_agent_node(&job, system, user).await?
    } else {
        run_llm_node(&job, system, user).await?
    };

    let verdict_json = verdict.as_ref().and_then(|v| serde_json::to_string(v).ok());
    Ok(NodeOutcome {
        artifact: NodeArtifact {
            hash: job.hash.clone(),
            content,
            verdict: verdict_json,
            model: Some(job.model.clone()),
            tokens_in: tokens.0,
            tokens_out: tokens.1,
            cost: tokens.2,
            elapsed_ms: started.elapsed().as_millis() as i64,
            created_at: chrono::Utc::now().timestamp_millis(),
        },
        verdict,
    })
}

const VERDICT_ASK: &str = "\n\nAfter your analysis — reasoning first — end the reply with exactly one fenced block:\n```json\n{\"pass\": true|false, \"score\": 0.0-1.0, \"issues\": [\"severity: what is wrong — how to fix it\"], \"summary\": \"one line\"}\n```\nPass only if the work genuinely meets the goal. Issues must be concrete and actionable; an empty list with pass=false is useless.";

fn build_system(job: &NodeJob) -> String {
    let mut s = format!(
        "You are the \"{}\" stage of an execution graph running inside a coding agent. \
         Stages before you have produced the inputs below; stages after you will consume \
         your reply verbatim as their input. Do exactly your stage's job — no preamble, \
         no meta-commentary about the pipeline.\n\nYour standing instruction:\n{}",
        job.node.label, job.node.prompt.trim()
    );
    if is_agentic(&job.node) {
        s.push_str(&format!("\n\nWork in {}.", job.cwd.display()));
    }
    if job.wants_verdict {
        s.push_str(VERDICT_ASK);
    }
    if job.attempt > 0 {
        s.push_str(&format!(
            "\n\nThis is attempt {} — feedback from the failed attempt is included; address it first.",
            job.attempt + 1
        ));
    }
    s
}

fn build_user_message(job: &NodeJob) -> String {
    let mut u = String::new();
    if !job.goal.trim().is_empty() {
        u.push_str(&format!("## The request\n\n{}\n\n", job.goal.trim()));
    }
    if !job.context.trim().is_empty() {
        u.push_str(&job.context);
    }
    if u.trim().is_empty() {
        u = "(no inputs reached this node — work from your standing instruction alone)".into();
    }
    u
}

/// One streaming completion, narrated into the chat exactly like a normal
/// run's turn: the same streaming events, the same persisted assistant
/// message with its reasoning. The graph orchestrates; the transcript stays
/// an ordinary chat.
async fn run_llm_node(
    job: &NodeJob,
    system: String,
    user: String,
) -> Result<(String, (i64, i64, f64), Option<Verdict>)> {
    let key = job.state.api_key()?;
    let client = job.state.client();

    let mut req = ChatRequest::new(
        job.model.clone(),
        vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})],
    );
    req.temperature = job.settings.temperature;
    if !job.settings.fallback_models.is_empty() {
        let mut chain = vec![job.model.clone()];
        chain.extend(job.settings.fallback_models.iter().cloned());
        req.models = Some(chain);
    }
    apply_effort(&mut req, job);

    let rctx = chat_ctx(job);
    let turn =
        crate::agent::runner::stream_turn(&rctx, &client, &key, &req, &job.settings, &job.model).await?;
    crate::agent::runner::persist_assistant(&rctx, &turn, &job.model)?;
    let usage = bump_usage(job, turn.usage.as_ref());
    if let Some(err) = &turn.error {
        return Err(AppError::msg(err.clone()));
    }
    let verdict = job.wants_verdict.then(|| parse_verdict(&turn.content)).flatten();
    Ok((turn.content, usage, verdict))
}

/// A `RunContext` view of this node's run — the key that unlocks the normal
/// chat protocol (streaming, persistence, tool rows) for graph nodes.
fn chat_ctx(job: &NodeJob) -> RunContext {
    RunContext {
        state: job.state.clone(),
        handle: job.handle.clone(),
        run_id: job.run_id.clone(),
        chat_id: job.chat_id.clone(),
    }
}

/// The bounded tool loop for agent-backed nodes — the subagent pattern, with
/// live events so the tile shows what its agent is doing.
async fn run_agent_node(
    job: &NodeJob,
    system: String,
    user: String,
) -> Result<(String, (i64, i64, f64), Option<Verdict>)> {
    let key = job.state.api_key()?;
    let client = job.state.client();
    let allowed = allowed_tools(&job.node, &job.settings);
    if allowed.is_empty() {
        return Err(AppError::msg(format!(
            "{} has no tools available in the current permission mode",
            job.node.label
        )));
    }
    let degraded = wanted_tools(&job.node) != allowed_level(&job.node, &job.settings);
    if degraded {
        let _ = job.handle.tx.send(AgentEvent::Notice {
            level: "warn".into(),
            text: format!(
                "{} is limited to {} tools by the \"{}\" permission mode.",
                job.node.label,
                allowed_level(&job.node, &job.settings),
                job.settings.permission_mode
            ),
        });
    }

    let specs: Vec<Value> = tools::registry()
        .iter()
        .filter(|t| allowed.contains(&t.name))
        .map(tools::ToolDef::to_spec)
        .collect();

    let env = ToolEnv {
        db: job.state.db.clone(),
        cwd: job.cwd.clone(),
        remote: job.remote.clone(),
        allowed_roots: job.allowed_roots.clone(),
        denied_paths: job.denied_paths.clone(),
        cancel: job.handle.cancel.clone(),
        timeout: Duration::from_millis(job.settings.tool_timeout_ms),
        sandbox_commands: job.settings.sandbox_commands,
        handle: job.handle.clone(),
        state: job.state.clone(),
        settings: job.settings.clone(),
        chat_id: job.chat_id.clone(),
        run_id: job.run_id.clone(),
    };

    let rctx = chat_ctx(job);
    let mut messages =
        vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})];
    let mut totals = (0i64, 0i64, 0f64);
    let steps = agent_steps(&job.node, &job.settings);
    let vision = model_supports_images(&job.state, &job.model);

    for step in 0..steps {
        if job.handle.cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        // Last step: take the tools away and demand the report. A stage that
        // loves browsing must still END with its findings — running out of
        // budget used to fail the whole run instead.
        let last = step + 1 == steps;
        if last {
            messages.push(json!({
                "role": "user",
                "content": "Your tool budget for this stage is exhausted. Do not request any more tools — write this stage's final report now from what you have gathered, and note anything that remains unverified."
            }));
        }
        let mut req = ChatRequest::new(job.model.clone(), messages.clone());
        if !last {
            req.tools = Some(specs.clone());
        }
        if !job.settings.fallback_models.is_empty() {
            let mut chain = vec![job.model.clone()];
            chain.extend(job.settings.fallback_models.iter().cloned());
            req.models = Some(chain);
        }
        apply_effort(&mut req, job);

        // The normal chat protocol, verbatim: the turn streams into the
        // transcript, persists as an assistant message, and every tool call
        // becomes a real tool row — approvals, journaling and all. A graph
        // run reads exactly like the same work done without the graph.
        let turn = crate::agent::runner::stream_turn(&rctx, &client, &key, &req, &job.settings, &job.model)
            .await?;
        let assistant_id = crate::agent::runner::persist_assistant(&rctx, &turn, &job.model)?;
        let u = bump_usage(job, turn.usage.as_ref());
        totals.0 += u.0;
        totals.1 += u.1;
        totals.2 += u.2;
        if let Some(err) = &turn.error {
            return Err(AppError::msg(err.clone()));
        }

        let calls: Vec<ToolCall> =
            turn.tool_calls.iter().filter(|c| !c.function.name.is_empty()).cloned().collect();
        if calls.is_empty() {
            let content = if turn.content.trim().is_empty() {
                "(this stage finished without producing a report)".to_string()
            } else {
                turn.content
            };
            let verdict = job.wants_verdict.then(|| parse_verdict(&content)).flatten();
            return Ok((content, totals, verdict));
        }

        crate::agent::runner::run_tool_calls(&rctx, &job.settings, &env, &assistant_id, &calls).await?;

        let mut assistant = json!({"role": "assistant", "tool_calls": serde_json::to_value(&calls)?});
        if !turn.content.is_empty() {
            assistant["content"] = json!(turn.content);
        }
        messages.push(assistant);

        // run_tool_calls persisted each result as a tool message; read them
        // back so this node's private conversation carries the outputs into
        // its next step — a node does not feed on the whole chat history.
        let ids: std::collections::HashSet<&str> = calls.iter().map(|c| c.id.as_str()).collect();
        let mut shots: Vec<String> = Vec::new();
        for m in job.state.db.list_messages(&job.chat_id)? {
            if m.role == "tool" {
                if let Some(cid) = m.tool_call_id.as_deref() {
                    if ids.contains(cid) {
                        if let Some(p) = crate::tools::inline_image(&m.content) {
                            shots.push(p);
                        }
                        messages.push(json!({"role": "tool", "tool_call_id": cid, "content": m.content}));
                    }
                }
            }
        }
        // A screenshot the model can actually look at beats a thousand words
        // of DOM dump. Only for models the catalogue says accept images.
        if vision && !shots.is_empty() {
            let mut parts = vec![json!({
                "type": "text",
                "text": "The screenshot(s) just captured, attached for visual inspection — judge the pixels:"
            })];
            for p in shots.iter().take(2) {
                if let Some(url) = image_data_url(p) {
                    parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
            }
            if parts.len() > 1 {
                messages.push(json!({"role": "user", "content": parts}));
            }
        }
    }

    Err(AppError::msg(format!("hit its step limit ({steps}) before finishing")))
}

/// Re-scans the workspace and rewrites `.strawberry/architecture.md`, then
/// emits the fresh map — so downstream stages (and the next chat turn) see
/// reality, not last week's structure. Enrichment is hash-gated upstream, so
/// only changed functions cost tokens.
async fn map_update_node(job: &NodeJob, started: Instant) -> Result<NodeOutcome> {
    let handle = job.handle.clone();
    let nid = job.node.id.clone();
    let doc = crate::analysis::run_scan(
        job.state.clone(),
        &job.chat_id,
        true,
        Some(job.model.clone()),
        None,
        Some(job.run_id.clone()),
        usize::MAX,
        job.handle.cancel.child_token(),
        move |ev| {
            if let crate::analysis::AnalysisEvent::Phase { message, .. } = ev {
                let _ = handle.tx.send(AgentEvent::GraphNode {
                    node_id: nid.clone(),
                    status: "running".into(),
                    detail: Some(message),
                });
            }
        },
    )
    .await?;
    let map = crate::analysis::prompt_context(&job.cwd).unwrap_or_default();
    let content = format!(
        "Architecture map refreshed: {} files, {} functions.\n\n{map}",
        doc.file_count, doc.fn_count
    );
    Ok(NodeOutcome {
        artifact: NodeArtifact {
            hash: job.hash.clone(),
            content,
            verdict: None,
            model: Some(job.model.clone()),
            tokens_in: 0,
            tokens_out: 0,
            cost: 0.0,
            elapsed_ms: started.elapsed().as_millis() as i64,
            created_at: chrono::Utc::now().timestamp_millis(),
        },
        verdict: None,
    })
}

/// Parks the run on the question machinery until the user answers: the card
/// renders in the chat, the graph tile shows "waiting". Shared by gates and
/// the design node's approval step.
async fn ask_user_choice(
    job: &NodeJob,
    question: String,
    options: Vec<String>,
    recommended: &str,
) -> Result<String> {
    let id = crate::db::new_id();
    let (tx, rx) = oneshot::channel();
    job.handle.questions.lock().insert(id.clone(), tx);

    let _ = job.handle.tx.send(AgentEvent::UserQuestion {
        id: id.clone(),
        question,
        options,
        recommended: Some(recommended.to_string()),
        allow_free_text: true,
    });
    let _ = job.handle.tx.send(AgentEvent::GraphNode {
        node_id: job.node.id.clone(),
        status: "waiting".into(),
        detail: Some("waiting for you".into()),
    });

    tokio::select! {
        biased;
        _ = job.handle.cancel.cancelled() => {
            job.handle.questions.lock().remove(&id);
            Err(AppError::Cancelled)
        }
        a = rx => a.map_err(|_| AppError::Cancelled),
    }
}

/// A gate parks the run on the existing question machinery: the card renders
/// in the chat, the answer routes the approve/reject ports.
async fn gate_node(job: &NodeJob) -> Result<NodeOutcome> {
    let preview: String = job.context.chars().take(1200).collect();
    let question = if job.node.prompt.trim().is_empty() {
        format!("Gate \"{}\": the work so far is below. Continue?\n\n{preview}", job.node.label)
    } else {
        format!("{}\n\n{preview}", job.node.prompt.trim())
    };
    let answer =
        ask_user_choice(job, question, vec!["Approve".into(), "Reject".into()], "Approve").await?;

    let approved = answer.trim().eq_ignore_ascii_case("approve");
    let verdict = Verdict {
        pass: approved,
        score: None,
        issues: if approved || answer.trim().eq_ignore_ascii_case("reject") {
            Vec::new()
        } else {
            vec![answer.trim().to_string()]
        },
        summary: if approved { "approved".into() } else { "rejected".into() },
    };
    let content =
        if job.context.trim().is_empty() { job.goal.clone() } else { job.context.trim().to_string() };
    Ok(NodeOutcome {
        artifact: NodeArtifact {
            hash: String::new(),
            content,
            verdict: serde_json::to_string(&verdict).ok(),
            model: None,
            tokens_in: 0,
            tokens_out: 0,
            cost: 0.0,
            elapsed_ms: 0,
            created_at: chrono::Utc::now().timestamp_millis(),
        },
        verdict: Some(verdict),
    })
}

/// The sketch node: the run parks, the sketch pad opens, the human draws.
/// Whatever they send — the layout description of their drawing — becomes the
/// artifact flowing downstream, bundled with the upstream inputs it responds
/// to. Free, and cached like any pure node: an unchanged goal reuses the
/// sketch instead of asking again ("draw once").
async fn sketch_node(job: &NodeJob) -> Result<NodeOutcome> {
    let id = crate::db::new_id();
    let (tx, rx) = oneshot::channel();
    job.handle.questions.lock().insert(id.clone(), tx);

    let _ = job.handle.tx.send(AgentEvent::SketchRequest {
        id: id.clone(),
        node_id: job.node.id.clone(),
        prompt: if job.node.prompt.trim().is_empty() {
            "Draw what you want built — boxes for regions, arrows for moves, text for notes.".to_string()
        } else {
            job.node.prompt.trim().to_string()
        },
    });
    let _ = job.handle.tx.send(AgentEvent::GraphNode {
        node_id: job.node.id.clone(),
        status: "waiting".into(),
        detail: Some("waiting for your sketch".into()),
    });

    let answer = tokio::select! {
        biased;
        _ = job.handle.cancel.cancelled() => {
            job.handle.questions.lock().remove(&id);
            return Err(AppError::Cancelled);
        }
        a = rx => a.map_err(|_| AppError::Cancelled)?,
    };

    let mut content = String::from("## The user's sketch\n\n");
    content.push_str(answer.trim());
    Ok(NodeOutcome {
        artifact: NodeArtifact {
            hash: job.hash.clone(),
            content,
            verdict: None,
            model: None,
            tokens_in: 0,
            tokens_out: 0,
            cost: 0.0,
            elapsed_ms: 0,
            created_at: chrono::Utc::now().timestamp_millis(),
        },
        verdict: None,
    })
}

/// The design stage is a conversation, not a decree: each proposal renders in
/// the chat as a real palette and wireframe (the fenced JSON draws itself),
/// then the run parks until the user approves it, asks for another take, or
/// types concrete feedback — which becomes the revision brief. Bounded so an
/// undecided user cannot loop the meter forever.
async fn design_node(job: &NodeJob) -> Result<(String, (i64, i64, f64), Option<Verdict>)> {
    const ROUNDS: u32 = 3;
    let mut totals = (0i64, 0i64, 0f64);
    let mut revision: Option<String> = None;

    for round in 0..ROUNDS {
        let mut user = build_user_message(job);
        if let Some(r) = &revision {
            user.push_str("\n\n## Revision request\n");
            user.push_str(r);
        }
        let system = build_system(job);
        let (content, t, _) = if is_agentic(&job.node) {
            run_agent_node(job, system, user).await?
        } else {
            run_llm_node(job, system, user).await?
        };
        totals.0 += t.0;
        totals.1 += t.1;
        totals.2 += t.2;

        if round + 1 == ROUNDS {
            let _ = job.handle.tx.send(AgentEvent::Notice {
                level: "info".into(),
                text: "Design round limit reached — continuing with this one.".into(),
            });
            return Ok((content, totals, None));
        }

        let answer = ask_user_choice(
            job,
            "The design proposal is above — palette and wireframe. Approve it, ask for another take, or describe what to change.".into(),
            vec!["Approve".into(), "Generate another".into()],
            "Approve",
        )
        .await?;
        let a = answer.trim().to_lowercase();
        if a == "approve" || a.is_empty() {
            return Ok((content, totals, None));
        }
        revision = Some(if a == "generate another" {
            "The user wants a distinctly different direction: change the palette family, the mood and the layout structure — not a variation of the same idea.".to_string()
        } else {
            answer.trim().to_string()
        });
    }
    unreachable!("design rounds always return");
}

// -------------------------------------------------------------------- helpers

fn apply_effort(req: &mut ChatRequest, job: &NodeJob) {
    if let Some(r) = effort_json(&job.node, &job.settings) {
        req.reasoning = Some(r);
    }
}

/// Per-node effort wins; "off" suppresses reasoning; unset inherits the
/// global setting. Traces stream under the same `show_reasoning` policy as a
/// normal chat: they are generated and billed either way, and excluding them
/// here made a thinking model's graph run look frozen for its entire
/// reasoning phase — the one thing a pipeline must never look like.
fn effort_json(node: &GraphNode, settings: &Settings) -> Option<Value> {
    let effort = match node.effort.as_deref() {
        Some("off") => None,
        Some(e) => Some(e.to_string()),
        None => settings.reasoning_effort.clone(),
    };
    effort.map(|e| json!({"effort": e, "exclude": !settings.show_reasoning}))
}

/// True when the cached model catalogue says this model accepts image input.
/// No catalogue → assume not, and rely on the rendered-text fallback.
fn model_supports_images(state: &AppState, model: &str) -> bool {
    let Some((payload, _)) = state.db.cached_models().ok().flatten() else { return false };
    let Ok(list) = serde_json::from_str::<Vec<crate::openrouter::ModelInfo>>(&payload) else {
        return false;
    };
    list.iter().find(|m| m.id == model).map(crate::openrouter::ModelInfo::supports_images).unwrap_or(false)
}

const MAX_VISION_BYTES: u64 = 6 * 1024 * 1024;

fn image_data_url(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_VISION_BYTES {
        return None;
    }
    crate::agent::prompt::data_url(path, crate::tools::image_mime(path))
}

// ------------------------------------------------------------ node refinement

pub struct GraphRefine {
    pub ctx: RunContext,
    pub graph_id: String,
    pub node_id: String,
    pub instruction: String,
}

/// Re-works one node's cached output under a user instruction — "warmer
/// palette", "shorter plan" — without re-running anything else. One model
/// call over the previous output; the revised artifact replaces the cache.
pub async fn execute_refine(r: GraphRefine) {
    let ctx = &r.ctx;
    let outcome = refine_inner(&r).await;
    let (status, error) = match &outcome {
        Ok(()) => ("complete", None),
        Err(AppError::Cancelled) => ("cancelled", None),
        Err(e) => ("error", Some(e.to_string())),
    };
    let _ = ctx.state.db.finish_run(&ctx.run_id, status, error.as_deref());
    ctx.emit(AgentEvent::RunFinished { run_id: ctx.run_id.clone(), status: status.into(), error });
    ctx.state.runs.finish(&ctx.run_id);
}

async fn refine_inner(r: &GraphRefine) -> Result<()> {
    let ctx = &r.ctx;
    let state = &ctx.state;
    let settings = state.settings();
    let key = state.api_key()?;
    let client = state.client();

    let row = state.db.get_graph(&r.graph_id)?;
    let spec: GraphSpec = serde_json::from_str(&row.spec)
        .map_err(|e| AppError::msg(format!("this graph cannot be read: {e}")))?;
    let mut artifacts: HashMap<String, NodeArtifact> =
        serde_json::from_str(&row.artifacts).unwrap_or_default();
    let node = spec
        .nodes
        .iter()
        .find(|n| n.id == r.node_id)
        .ok_or_else(|| AppError::msg("that node no longer exists"))?;
    let previous = artifacts
        .get(&r.node_id)
        .filter(|a| !a.content.trim().is_empty())
        .ok_or_else(|| AppError::msg("nothing to refine yet — run the graph first"))?
        .clone();

    let chat = state.db.get_chat(&ctx.chat_id)?;
    let model = node
        .model
        .clone()
        .or(chat.model.clone())
        .unwrap_or_else(|| settings.default_model.clone());

    ctx.emit(AgentEvent::RunStarted {
        run_id: ctx.run_id.clone(),
        chat_id: ctx.chat_id.clone(),
        model: format!("refine:{}", node.label),
    });
    ctx.emit(AgentEvent::GraphNode {
        node_id: node.id.clone(),
        status: "running".into(),
        detail: Some("refining".into()),
    });

    let system = format!(
        "You are refining the output of the \"{}\" stage of an execution graph. Apply the \
         refinement instruction to the previous output. Keep everything that was not asked to \
         change, keep the same format, and keep any fenced JSON blocks present and updated. \
         Reply with the complete revised output only — no commentary about the change.{}",
        node.label,
        if is_verdict(node) { VERDICT_ASK } else { "" }
    );
    let user = format!(
        "## Previous output\n\n{}\n\n## Refinement instruction\n\n{}",
        previous.content,
        r.instruction.trim()
    );

    let mut req = ChatRequest::new(
        model.clone(),
        vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})],
    );
    req.temperature = settings.temperature;
    if !settings.fallback_models.is_empty() {
        let mut chain = vec![model.clone()];
        chain.extend(settings.fallback_models.iter().cloned());
        req.models = Some(chain);
    }
    if let Some(rj) = effort_json(node, &settings) {
        req.reasoning = Some(rj);
    }

    let started = Instant::now();
    let mut buf = String::new();
    let mut last = Instant::now();
    let nid = node.id.clone();
    let handle = ctx.handle.clone();
    let mut on = |ev: StreamEvent| {
        let t = match ev {
            StreamEvent::Content(t) | StreamEvent::Reasoning(t) => t,
            _ => return,
        };
        buf.push_str(&t);
        if buf.len() >= 1536 || last.elapsed() >= Duration::from_millis(48) {
            last = Instant::now();
            let _ = handle
                .tx
                .send(AgentEvent::GraphNodeDelta { node_id: nid.clone(), text: std::mem::take(&mut buf) });
        }
    };
    let turn = client.stream_chat(&key, &req, &ctx.handle.cancel, settings.max_retries, &mut on).await?;
    if !buf.is_empty() {
        ctx.emit(AgentEvent::GraphNodeDelta { node_id: node.id.clone(), text: buf });
    }
    if let Some(err) = &turn.error {
        return Err(AppError::msg(err.clone()));
    }
    let usage = turn.usage.as_ref();
    if let Some(u) = usage {
        let _ = state.db.bump_run_usage(&ctx.run_id, u.prompt_tokens, u.completion_tokens, u.cost, 1);
        ctx.emit(AgentEvent::Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            cost: u.cost,
        });
    }

    let verdict = is_verdict(node).then(|| parse_verdict(&turn.content)).flatten();
    let art = NodeArtifact {
        // A refined output no longer matches any input hash: only a pin (the
        // frontend sets one) protects it from the next full run.
        hash: "refined".into(),
        content: turn.content,
        verdict: verdict.as_ref().and_then(|v| serde_json::to_string(v).ok()),
        model: Some(model),
        tokens_in: usage.map(|u| u.prompt_tokens).unwrap_or(0),
        tokens_out: usage.map(|u| u.completion_tokens).unwrap_or(0),
        cost: usage.map(|u| u.cost).unwrap_or(0.0),
        elapsed_ms: started.elapsed().as_millis() as i64,
        created_at: chrono::Utc::now().timestamp_millis(),
    };
    emit_done(ctx, &node.id, "done", &art, verdict.as_ref());
    artifacts.insert(node.id.clone(), art);
    if let Ok(json) = serde_json::to_string(&artifacts) {
        let _ = state.db.set_graph_artifacts(&r.graph_id, &json);
    }
    Ok(())
}

fn bump_usage(job: &NodeJob, usage: Option<&Usage>) -> (i64, i64, f64) {
    let Some(u) = usage else { return (0, 0, 0.0) };
    let _ = job.state.db.bump_run_usage(&job.run_id, u.prompt_tokens, u.completion_tokens, u.cost, 1);
    let _ = job.handle.tx.send(AgentEvent::Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        cost: u.cost,
    });
    (u.prompt_tokens, u.completion_tokens, u.cost)
}

/// Delivers cached or synthesized output to the transcript in one piece.
fn deliver_to_chat(ctx: &RunContext, content: &str, model: &str) -> Result<()> {
    let id = crate::db::new_id();
    ctx.emit(AgentEvent::MessageStarted { message_id: id.clone(), role: "assistant".into() });
    ctx.state.db.insert_message(&NewMessage {
        id: Some(id.clone()),
        chat_id: ctx.chat_id.clone(),
        role: "assistant".into(),
        content: content.to_string(),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
        model: Some(model.to_string()),
        status: "complete".into(),
        error: None,
        usage: None,
        run_id: Some(ctx.run_id.clone()),
    })?;
    ctx.emit(AgentEvent::MessageFinished {
        message_id: id,
        content: content.to_string(),
        reasoning: None,
        finish_reason: None,
        usage: None,
    });
    Ok(())
}

fn emit_done(ctx: &RunContext, nid: &str, status: &str, art: &NodeArtifact, verdict: Option<&Verdict>) {
    ctx.emit(AgentEvent::GraphNodeDone {
        node_id: nid.to_string(),
        status: status.to_string(),
        artifact: serde_json::to_value(art).unwrap_or(Value::Null),
        verdict: verdict.and_then(|v| serde_json::to_value(v).ok()),
    });
}

fn wanted_tools(node: &GraphNode) -> &'static str {
    match node.config.tools.as_deref() {
        Some("none") => "none",
        Some("read") => "read",
        Some("web") => "web",
        Some("edit") => "edit",
        Some("full") => "full",
        _ => default_tools(&node.kind),
    }
}

/// The level a node actually gets after the permission mode has its say: a
/// graph must never block on an approval card per tool call, so a mode that
/// would prompt degrades the level instead — the subagent rule.
fn allowed_level(node: &GraphNode, settings: &Settings) -> &'static str {
    let want = wanted_tools(node);
    match settings.mode() {
        PermissionMode::Auto => want,
        PermissionMode::AcceptEdits => match want {
            "full" => "edit",
            w => w,
        },
        _ => match want {
            "edit" | "full" => "read",
            w => w,
        },
    }
}

fn allowed_tools(node: &GraphNode, settings: &Settings) -> Vec<&'static str> {
    // view_image rides at read level: even a degraded verifier can still LOOK
    // at the images that exist.
    let read = vec!["read_file", "list_dir", "glob", "grep", "view_image"];
    let mut v = match allowed_level(node, settings) {
        "web" => vec!["web_search", "fetch_url"],
        "read" => read,
        "edit" => {
            let mut v = read;
            v.extend(["write_file", "edit_file"]);
            v
        }
        "full" => {
            let mut v = read;
            v.extend([
                "write_file",
                "edit_file",
                "run_command",
                "run_python",
                "web_search",
                "fetch_url",
                "download_file",
                "web_screenshot",
                "share_image",
                "shell_start",
                "shell_send",
                "shell_kill",
                "shell_read",
                "shell_list",
            ]);
            v
        }
        _ => Vec::new(),
    };
    // The memory toggle lets a stage consult past conversations.
    if node.config.memory == Some(true) && !v.is_empty() {
        v.extend(["memory_search", "memory_read_chat"]);
    }
    v
}

fn agent_steps(node: &GraphNode, settings: &Settings) -> u32 {
    match node.kind.as_str() {
        "execute" => settings.max_steps.max(12),
        "optimize" | "refactor" => settings.max_steps.clamp(12, 30),
        "research" | "fetch" => 14,
        "codebase" | "environment" => 12,
        "test" | "documentation" => 14,
        // A verifier that builds a harness, runs it and screenshots the result
        // needs more room than a critic that reads.
        "verify" => 16,
        _ => 10,
    }
}


/// Pulls the verdict block out of a reply: the last fenced JSON, or the last
/// bare object mentioning "pass". A verifier that failed to produce one
/// passes-with-a-warning rather than looping the graph on noise.
fn parse_verdict(content: &str) -> Option<Verdict> {
    let candidate = last_fenced_json(content).or_else(|| last_object_with(content, "\"pass\""))?;
    let v: Value = serde_json::from_str(&candidate).ok()?;
    let pass = v.get("pass").and_then(Value::as_bool)?;
    let issues = v
        .get("issues")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|i| match i {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Verdict {
        pass,
        score: v.get("score").and_then(Value::as_f64),
        issues,
        summary: v.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

fn last_fenced_json(content: &str) -> Option<String> {
    let mut found = None;
    let mut rest = content;
    while let Some(start) = rest.find("```json") {
        let body = &rest[start + 7..];
        if let Some(end) = body.find("```") {
            found = Some(body[..end].trim().to_string());
            rest = &body[end + 3..];
        } else {
            break;
        }
    }
    found
}

fn last_object_with(content: &str, needle: &str) -> Option<String> {
    let at = content.rfind(needle)?;
    let start = content[..at].rfind('{')?;
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(content[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------- auto planner

/// The node catalogue as the planner model sees it. Kept honest by hand: this
/// mirrors the frontend catalogue in `lib/meg.ts`.
const PLANNER_CATALOG: &str = r#"Node kinds you may use:
- goal — entry point; restates the request as acceptance criteria (leave `prompt` empty to pass the message through free).
- research — agent with web search: prior art, references, techniques. Produces knowledge, not files.
- fetch — agent that ACQUIRES from the internet: downloads reference images, example code/repos, libraries, datasets and test fixtures into the workspace and reports a manifest of local paths. Use whenever the task needs real assets on disk (an image to edit and its mask, a reference design to imitate, sample data to run against); wire it into execute and verify.
- codebase — agent that reads the workspace: files, conventions, integration points.
- environment — agent that works out how to build/run/test here.
- archmap — emits the saved architecture map, free. Only useful for refactor/optimize flows.
- plan — turns goal + context into an ordered plan (single call, no tools).
- architecture — designs modules/data flow/interfaces (single call).
- design — sets visual direction and emits a colour theme + wireframe (single call). Use for anything with a visual surface.
- sketch — NOT a model: the run pauses and the user's sketch pad opens. The HUMAN draws the wireframe or pastes a screenshot and annotates it; the drawing's layout description flows downstream. Free. Place it after design (or right after goal) on visual tasks where the user's own layout intent matters, wired into execute.
- brainstorm — several genuinely different approaches (single call).
- judge — picks one approach from brainstormed alternatives (single call).
- execute — THE HANDS: full tool-using agent that does the work.
- optimize / refactor / documentation — editing agents for those jobs.
- test — agent that runs tests/build; verdict node with pass/fail ports.
- verify — agent that exercises the work (runs it, screenshots UIs, builds harnesses, downloads fixtures) and judges it against the goal and any wired-in criteria; verdict node with pass/fail ports.
- critique — non-gating review feeding guidance forward (single call).
- edgecases — enumerates failure modes (single call).
- merge — joins branches, free.
- gate — pauses for the user's approve/reject.
- output — composes the final answer to the user. Every graph should end in one.
- note — a free text block you write, wired in as standing context."#;

const PLANNER_RULES: &str = r#"Design the smallest graph that genuinely fits the task — 3 nodes for a simple question, 6-9 for a real build. Rules:
- Edges carry outputs downstream. `port` is "out" for plain nodes; "pass"/"fail" for test/verify; "approve"/"reject" for gate.
- A verify or test node should loop back to the stage that fixes problems: add an edge {from: verifier, to: fixer, port: "fail", loop: true, maxIters: 2-3}, plus {port: "pass"} onward to output.
- Anything visual deserves design before execute, and a verify that checks the design was applied.
- Simple questions need NO graph machinery: goal(empty prompt) → execute (or a single reasoning node) → output.
- Whenever the plan has a verify or test stage, ALSO include one note node labelled "Criteria": its `prompt` is your checklist of the concrete, checkable requirements this specific request implies (visual rules, must-pass behaviours, formats). Wire it into the verifier so the checks are explicit, and the user can edit them.
- Prefer the user's saved composite stages (listed below, if any) when one fits — they encode how this user likes a job done.
- Per node, set `model` to a cheaper id from the roster for mechanical stages (research, codebase, docs) and a stronger one for execute/architecture — or null to inherit the chat's model. Only use ids from the roster: every one of them reads images as well as text, which verification's screenshots require.
- Set `effort` ("low"|"medium"|"high") only where it earns its cost; null otherwise.
- Write a concise `prompt` (standing instruction, 2-5 lines) for every node that thinks or acts; null lets the app use its stock instruction for that kind, which is usually right.

Reply with exactly one fenced ```json block:
{"name": "short graph name", "rationale": "1-3 sentences on why this shape and these models", "nodes": [{"id": "n1", "kind": "goal", "label": null, "prompt": null, "model": null, "effort": null, "tools": null}], "edges": [{"from": "n1", "to": "n2", "port": "out", "loop": false, "maxIters": 3}]}"#;

/// One model call that drafts a graph for a goal. Returns the raw plan JSON —
/// the frontend materialises it into a real spec (default prompts, layout) and
/// the user approves it before anything runs. `customs` describes the user's
/// saved composite nodes so the planner can reuse them.
pub async fn plan_graph(
    state: &Arc<AppState>,
    chat_id: &str,
    goal: &str,
    customs: Option<Value>,
) -> Result<Value> {
    let settings = state.settings();
    let key = state.api_key()?;
    let client = state.client();
    let chat = state.db.get_chat(chat_id)?;
    let model = chat.model.clone().unwrap_or_else(|| settings.default_model.clone());

    // The roster the planner may assign from: only models that read BOTH text
    // and images, because verification feeds screenshots back to its stages.
    let mut roster: Vec<String> = vec![format!("- {model} (the chat's model — the default)")];
    if let Ok(Some((payload, _))) = state.db.cached_models() {
        if let Ok(list) = serde_json::from_str::<Vec<crate::openrouter::ModelInfo>>(&payload) {
            let mut priced: Vec<(f64, &crate::openrouter::ModelInfo)> = list
                .iter()
                .filter(|m| {
                    m.supports_tools() && m.supports_images() && m.context_length.unwrap_or(0) >= 32_000
                })
                .filter_map(|m| {
                    let p = m.pricing.as_ref()?.prompt.parse::<f64>().ok()?;
                    (p > 0.0).then_some((p, m))
                })
                .collect();
            priced.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (price, m) in priced.iter().take(6) {
                if m.id != model {
                    roster.push(format!("- {} (vision, ${:.2}/M in)", m.id, price * 1_000_000.0));
                }
            }
        }
    }

    let mut custom_section = String::new();
    if let Some(arr) = customs.as_ref().and_then(Value::as_array) {
        let mut lines: Vec<String> = Vec::new();
        for c in arr.iter().take(12) {
            let Some(name) = c.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty())
            else {
                continue;
            };
            let stages = c
                .get("stages")
                .and_then(Value::as_array)
                .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" → "))
                .unwrap_or_default();
            lines.push(if stages.is_empty() {
                format!("- custom:{name}")
            } else {
                format!("- custom:{name} (inside: {stages})")
            });
        }
        if !lines.is_empty() {
            custom_section = format!(
                "\n\nThe user's saved composite stages — reuse one as a single node by setting its `kind` exactly as written:\n{}",
                lines.join("\n")
            );
        }
    }

    let system = format!(
        "You are the graph planner inside a coding agent. Given the user's request, design a Model \
         Execution Graph — a pipeline of stages that will answer it well without wasting tokens.\n\n\
         {PLANNER_CATALOG}{custom_section}\n\nModels available (use these ids only — all of them read images):\n{}\n\n{PLANNER_RULES}",
        roster.join("\n")
    );

    let mut req = ChatRequest::new(
        model.clone(),
        vec![
            json!({"role": "system", "content": system}),
            json!({"role": "user", "content": format!("The request to plan for:\n\n{goal}")}),
        ],
    );
    req.temperature = Some(0.2);
    req.max_tokens = Some(4000);
    req.reasoning = Some(json!({ "exclude": true }));
    if !settings.fallback_models.is_empty() {
        let mut chain = vec![model.clone()];
        chain.extend(settings.fallback_models.iter().cloned());
        req.models = Some(chain);
    }

    let cancel = tokio_util::sync::CancellationToken::new();
    let mut sink = |_: StreamEvent| {};
    let turn = client.stream_chat(&key, &req, &cancel, settings.max_retries.min(1), &mut sink).await?;
    if let Some(err) = &turn.error {
        return Err(AppError::msg(err.clone()));
    }

    let raw = last_fenced_json(&turn.content)
        .or_else(|| last_object_with(&turn.content, "\"nodes\""))
        .ok_or_else(|| AppError::msg("the planner did not return a graph — try rephrasing the goal"))?;
    let plan: Value =
        serde_json::from_str(&raw).map_err(|e| AppError::msg(format!("the planner's graph is unreadable: {e}")))?;
    let ok = plan.get("nodes").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    if !ok {
        return Err(AppError::msg("the planner returned an empty graph — try rephrasing the goal"));
    }
    Ok(plan)
}
