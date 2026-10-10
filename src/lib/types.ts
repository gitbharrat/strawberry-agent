import { errText } from "~/lib/err";

// Wire types. These mirror the Rust structs exactly; the backend is the source
// of truth, so treat any mismatch here as a bug in this file.

export interface AppInfo {
  version: string;
  platform: string;
  arch: string;
  dataDir: string;
  hasApiKey: boolean;
}

export interface Settings {
  theme: "system" | "light" | "dark";
  accent: string;
  density: "comfortable" | "compact";
  uiFontSize: number;
  zoom: number;
  reduceMotion: boolean;
  contentWidth: number;

  defaultModel: string;
  fallbackModels: string[];
  temperature: number | null;
  maxTokens: number | null;
  topP: number | null;
  reasoningEffort: string | null;
  showReasoning: boolean;
  systemPrompt: string | null;

  toolsEnabled: boolean;
  enabledTools: string[];
  maxSteps: number;
  /** "readOnly" | "ask" | "acceptEdits" | "auto" */
  permissionMode: string;
  sandboxFiles: boolean;
  sandboxCommands: boolean;
  allowedRoots: string[];
  deniedPaths: string[];
  toolTimeoutMs: number;
  autoCompactPct: number;

  requestTimeoutMs: number;
  connectTimeoutMs: number;
  maxRetries: number;
  lowBandwidth: boolean;
  sendAttribution: boolean;
  historyLimit: number;

  streamCoalesceMs: number;

  shell: string | null;
  terminalFontFamily: string;
  terminalFontSize: number;
  terminalScrollback: number;

  maxUploadMb: number;
  /** "file" | "keychain" */
  secretStore: string;
}

export interface Project {
  id: string;
  name: string;
  rootPath: string | null;
  color: string | null;
  icon: string | null;
  systemPrompt: string | null;
  createdAt: number;
  updatedAt: number;
  archived: boolean;
  sortOrder: number;
  chatCount: number;
}

export interface Chat {
  id: string;
  projectId: string | null;
  title: string;
  model: string | null;
  systemPrompt: string | null;
  cwd: string | null;
  createdAt: number;
  updatedAt: number;
  archived: boolean;
  pinned: boolean;
  totalCost: number;
  messageCount: number;
}

export type MessageRole = "user" | "assistant" | "tool" | "system";

export interface Message {
  id: string;
  chatId: string;
  seq: number;
  role: MessageRole;
  content: string;
  reasoning: string | null;
  toolCalls: string | null;
  toolCallId: string | null;
  model: string | null;
  status: string;
  error: string | null;
  usage: string | null;
  runId: string | null;
  createdAt: number;
}

export interface Attachment {
  id: string;
  chatId: string;
  messageId: string | null;
  name: string;
  mime: string;
  size: number;
  path: string;
  kind: "image" | "pdf" | "text" | string;
  createdAt: number;
}

export interface ToolCallRecord {
  id: string;
  runId: string;
  chatId: string;
  messageId: string | null;
  name: string;
  args: string;
  result: string | null;
  status: string;
  error: string | null;
  startedAt: number;
  finishedAt: number | null;
}

export interface Run {
  id: string;
  chatId: string;
  status: string;
  model: string | null;
  steps: number;
  tokensIn: number;
  tokensOut: number;
  cost: number;
  error: string | null;
  startedAt: number;
  finishedAt: number | null;
}

export interface ModelInfo {
  id: string;
  name: string;
  description: string;
  contextLength: number | null;
  created: number | null;
  architecture: {
    modality: string;
    inputModalities: string[];
    outputModalities: string[];
    tokenizer: string | null;
  } | null;
  pricing: {
    prompt: string;
    completion: string;
    image: string | null;
    inputCacheRead: string | null;
    inputCacheWrite: string | null;
  } | null;
  supportedParameters: string[];
  topProvider: {
    contextLength: number | null;
    maxCompletionTokens: number | null;
    isModerated: boolean | null;
  } | null;
  reasoning: {
    mandatory: boolean;
    defaultEnabled: boolean;
    supportedEfforts: string[];
    defaultEffort: string | null;
  } | null;
}

export interface PlanDoc {
  title: string;
  markdown: string;
  updatedAt: number;
}

export interface ShellInfo {
  id: string;
  command: string;
  alive: boolean;
  bufferedBytes: number;
}

export interface WorkspaceStatus {
  cwd: string | null;
  exists: boolean;
  isRepo: boolean;
  branch: string | null;
  detached: boolean;
  dirty: number | null;
  gitAvailable: boolean;
}

export interface ModelCatalog {
  models: ModelInfo[];
  fetchedAt: number;
  stale: boolean;
}

export interface SshHost {
  id: string;
  label: string;
  host: string;
  port: number;
  username: string;
  auth: "password" | "key" | string;
  keyPath: string | null;
  createdAt: number;
  lastUsed: number | null;
}

export interface SshConfigHost {
  alias: string;
  hostName: string | null;
  user: string | null;
  port: number | null;
  source: "project" | "user" | string;
}

export interface TerminalInfo {
  id: string;
  kind: "local" | "ssh";
  title: string;
  cwd: string | null;
  hostId: string | null;
  alive: boolean;
}

export interface Artifact {
  changeId: string;
  path: string;
  name: string;
  chatId: string;
  chatTitle: string;
  createdAt: number;
}

export interface SearchHit {
  chatId: string;
  chatTitle: string;
  messageId: string;
  role: string;
  snippet: string;
  createdAt: number;
}

export interface ToolSummary {
  name: string;
  description: string;
  risk: "read" | "write" | "exec" | "network";
}

export interface FsEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  modified: number;
  hidden: boolean;
  expandable: boolean;
}

export interface FileContents {
  path: string;
  name: string;
  content: string;
  language: string;
  size: number;
  lines: number;
  binary: boolean;
  truncated: boolean;
  readOnly: boolean;
  hash: string;
  modified: number;
}

export interface ChangedFile {
  path: string;
  name: string;
  latestChangeId: string;
  op: "create" | "modify" | "delete" | string;
  origin: "agent" | "manual" | string;
  edits: number;
  bytesBefore: number;
  bytesAfter: number;
  reverted: boolean;
  exists: boolean;
  updatedAt: number;
}

export interface ChangeDiff {
  path: string;
  language: string;
  before: string;
  after: string;
  reverted: boolean;
  op: string;
}

export interface RevertResult {
  path: string;
  restoredBytes: number;
  wasModifiedSince: boolean;
}

export interface KeyStatus {
  configured: boolean;
  masked: string | null;
  store: string;
  path: string;
}

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cost: number;
}

/** Discriminated union matching Rust's `AgentEvent`. */
export type AgentEvent =
  | { type: "runStarted"; runId: string; chatId: string; model: string }
  | { type: "stepStarted"; step: number }
  | { type: "messageStarted"; messageId: string; role: string }
  | { type: "delta"; messageId: string; text: string }
  | { type: "reasoning"; messageId: string; text: string }
  | { type: "toolCallStarted"; id: string; name: string }
  | { type: "toolCallArgs"; id: string; delta: string }
  | { type: "toolApproval"; id: string; name: string; args: unknown; risk: string; warning?: string | null }
  | { type: "planUpdated"; chatId: string; title: string; markdown: string }
  | {
      type: "userQuestion";
      id: string;
      question: string;
      options: string[];
      recommended: string | null;
      allowFreeText: boolean;
    }
  | { type: "toolStarted"; id: string; name: string; args: unknown }
  | {
      type: "toolFinished";
      id: string;
      name: string;
      ok: boolean;
      summary: string;
      display: Record<string, unknown> | null;
      durationMs: number;
    }
  | {
      type: "messageFinished";
      messageId: string;
      content: string;
      reasoning: string | null;
      finishReason: string | null;
      usage: Usage | null;
    }
  | { type: "usage"; promptTokens: number; completionTokens: number; cost: number }
  | { type: "retry"; attempt: number; delayMs: number; reason: string }
  | { type: "notice"; level: string; text: string }
  | { type: "titleChanged"; chatId: string; title: string }
  // ---- Model Execution Graph. Graph runs are ordinary runs; these extra
  // events ride the same channel and are consumed by the graph pane.
  | {
      type: "graphStarted";
      graphId: string;
      name: string;
      nodes: { id: string; label: string; kind: string }[];
    }
  | { type: "sketchRequest"; id: string; nodeId: string; prompt: string }
  | { type: "graphNode"; nodeId: string; status: string; detail: string | null }
  | { type: "graphNodeDelta"; nodeId: string; text: string }
  | { type: "graphNodeDone"; nodeId: string; status: string; artifact: unknown; verdict: unknown }
  | { type: "graphLoop"; edgeId: string; iteration: number; maxIters: number }
  | { type: "runFinished"; runId: string; status: string; error: string | null };

// ---- architecture analysis

export interface ArchFnLog {
  ts: number;
  /** "indexed" | "added" | "modified" | "removed <name>" */
  event: string;
  runId: string | null;
}

export interface ArchFn {
  name: string;
  line: number;
  endLine: number;
  params: string;
  exported: boolean;
  complexity: number;
  nesting: number;
  quality: number;
  lints: string[];
  calls: string[];
  tokens: string[];
  hash: string;
  hint: string | null;
  core: boolean;
  dead: boolean;
  stale: boolean;
  desc: string | null;
  inputs: string | null;
  outputs: string | null;
  tag: string | null;
  note: string | null;
  bigO: string | null;
  space: string | null;
  similar: { target: string; score: number }[];
  log: ArchFnLog[];
}

export interface ArchFile {
  id: string;
  lang: string;
  lines: number;
  hash: string;
  quality: number;
  lints: string[];
  role: string | null;
  kind: string | null;
  imports: string[];
  provides: string[];
  consumes: string[];
  usedBy: string[];
  inDegree: number;
  degree: number;
  hub: boolean;
  maxCx: number;
  functions: ArchFn[];
  log: ArchFnLog[];
}

export interface ArchEdge {
  from: string;
  to: string;
  weight: number;
  /** Channel labels ("route /api/x", "event a.b") when linked beyond imports. */
  via: string[];
}

export interface ArchDoc {
  version: number;
  root: string;
  generatedAt: number;
  model: string | null;
  enriched: boolean;
  fileCount: number;
  fnCount: number;
  files: ArchFile[];
  edges: ArchEdge[];
}

export interface AnalysisSnapshot {
  root: string | null;
  doc: ArchDoc | null;
}

export type AnalysisEvent =
  | { type: "phase"; phase: string; message: string }
  | { type: "progress"; done: number; total: number }
  | { type: "notice"; text: string };

export interface AppError {
  kind: string;
  message: string;
}

export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as AppError).message);
  return errText(e);
}

/** Where the context window goes, estimated from the real request parts. */
export interface ContextBreakdown {
  systemTokens: number;
  architectureTokens: number;
  toolsTokens: number;
  historyTokens: number;
  encodedMessages: number;
  storedMessages: number;
}
