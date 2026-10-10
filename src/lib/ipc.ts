// Typed wrappers over Tauri's `invoke`.
//
// Every backend call goes through here so the rest of the app never touches a
// stringly-typed command name, and so there is one place to add logging or
// error shaping.

import { invoke, Channel } from "@tauri-apps/api/core";
import type { GraphDoc, GraphSpec } from "./meg";
import type {
  ContextBreakdown,
  AgentEvent,
  AnalysisEvent,
  AnalysisSnapshot,
  AppInfo,
  ArchDoc,
  Artifact,
  Attachment,
  ChangeDiff,
  ChangedFile,
  Chat,
  FileContents,
  FsEntry,
  KeyStatus,
  ModelCatalog,
  Message,
  Project,
  RevertResult,
  Run,
  SearchHit,
  Settings,
  SshConfigHost,
  SshHost,
  TerminalInfo,
  ToolCallRecord,
  PlanDoc,
  ShellInfo,
  ToolSummary,
  WorkspaceStatus,
} from "./types";

export interface ProjectPatchInput {
  id: string;
  name?: string;
  rootPath?: string;
  color?: string;
  systemPrompt?: string;
  archived?: number;
}

export interface ChatPatchInput {
  id: string;
  title?: string;
  model?: string;
  systemPrompt?: string;
  cwd?: string;
  projectId?: string;
  archived?: number;
  pinned?: number;
}

export const api = {
  // ---- app
  appInfo: () => invoke<AppInfo>("app_info"),
  openDevtools: () => invoke<void>("open_devtools"),

  // ---- settings and credentials
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<Settings>("set_settings", { settings }),
  keyStatus: () => invoke<KeyStatus>("api_key_status"),
  setApiKey: (key: string) => invoke<KeyStatus>("set_api_key", { key }),
  clearApiKey: () => invoke<void>("clear_api_key"),
  verifyApiKey: (key?: string) => invoke<Record<string, unknown>>("verify_api_key", { key }),
  credits: () => invoke<Record<string, unknown>>("get_credits"),

  // ---- models
  models: (refresh = false) => invoke<ModelCatalog>("list_models", { refresh }),

  // ---- projects
  projects: () => invoke<Project[]>("list_projects"),
  createProject: (name: string, rootPath?: string) => invoke<Project>("create_project", { name, rootPath }),
  updateProject: (patch: ProjectPatchInput) => invoke<Project>("update_project", { patch }),
  deleteProject: (id: string) => invoke<void>("delete_project", { id }),

  // ---- chats
  chats: (projectId?: string | null) => invoke<Chat[]>("list_chats", { projectId }),
  createChat: (args: { projectId?: string | null; title?: string; model?: string; cwd?: string }) =>
    invoke<Chat>("create_chat", args),
  chat: (id: string) => invoke<Chat>("get_chat", { id }),
  updateChat: (patch: ChatPatchInput) => invoke<Chat>("update_chat", { patch }),
  deleteChat: (id: string) => invoke<void>("delete_chat", { id }),

  messages: (chatId: string) => invoke<Message[]>("list_messages", { chatId }),
  toolCalls: (chatId: string) => invoke<ToolCallRecord[]>("list_tool_calls", { chatId }),
  deleteMessage: (id: string) => invoke<void>("delete_message", { id }),
  updateMessage: (id: string, content: string) => invoke<Message>("update_message", { id, content }),
  search: (query: string, limit?: number) => invoke<SearchHit[]>("search_messages", { query, limit }),

  // ---- attachments
  attachFiles: (chatId: string, paths: string[]) => invoke<Attachment[]>("attach_files", { chatId, paths }),
  attachBytes: (chatId: string, name: string, mime: string, dataBase64: string) =>
    invoke<Attachment>("attach_bytes", { chatId, name, mime, dataBase64 }),
  attachments: (chatId: string) => invoke<Attachment[]>("list_attachments", { chatId }),

  // ---- runs
  sendMessage: (chatId: string, text: string, attachmentIds: string[], onEvent: Channel<AgentEvent>) =>
    invoke<string>("send_message", { chatId, text, attachmentIds, onEvent }),

  retryFrom: (chatId: string, seq: number, editedText: string | null, onEvent: Channel<AgentEvent>) =>
    invoke<string>("retry_from", { chatId, seq, editedText, onEvent }),

  subscribeRun: (runId: string, onEvent: Channel<AgentEvent>) =>
    invoke<void>("run_subscribe", { runId, onEvent }),

  cancelRun: (runId: string) => invoke<void>("cancel_run", { runId }),
  runSteer: (chatId: string, text: string) => invoke<Message>("run_steer", { chatId, text }),
  approveTool: (runId: string, callId: string, approved: boolean) =>
    invoke<void>("approve_tool", { runId, callId, approved }),
  getPlan: (chatId: string) => invoke<PlanDoc | null>("get_plan", { chatId }),
  compactChat: (chatId: string) =>
    invoke<{ removed: number; kept: number; summaryChars: number }>("compact_chat", { chatId }),
  contextBreakdown: (chatId: string) => invoke<ContextBreakdown>("context_breakdown", { chatId }),
  setRemoteWorkspace: (chatId: string, kind: "host" | "alias", value: string, dir: string) =>
    invoke<string>("set_remote_workspace", { chatId, kind, value, dir }),
  remoteCompleteDir: (chatId: string, kind: "host" | "alias", value: string, prefix: string) =>
    invoke<string[]>("remote_complete_dir", { chatId, kind, value, prefix }),
  answerQuestion: (runId: string, questionId: string, answer: string) =>
    invoke<void>("answer_question", { runId, questionId, answer }),
  bgShells: () => invoke<ShellInfo[]>("bg_shells"),
  bgShellKill: (id: string) => invoke<void>("bg_shell_kill", { id }),
  activeRuns: () => invoke<Run[]>("active_runs"),
  runningChatIds: () => invoke<string[]>("running_chat_ids"),

  // ---- terminals
  termOpenLocal: (
    args: { id?: string; cwd?: string; cols: number; rows: number },
    onData: Channel<ArrayBuffer>,
  ) => invoke<TerminalInfo>("term_open_local", { ...args, onData }),
  termOpenSsh: (
    args: { id?: string; hostId: string; cols: number; rows: number },
    onData: Channel<ArrayBuffer>,
  ) => invoke<TerminalInfo>("term_open_ssh", { ...args, onData }),
  termWrite: (id: string, data: string) => invoke<void>("term_write", { id, data }),
  termResize: (id: string, cols: number, rows: number) => invoke<void>("term_resize", { id, cols, rows }),
  termClose: (id: string) => invoke<void>("term_close", { id }),
  termList: () => invoke<TerminalInfo[]>("term_list"),

  // ---- ssh
  sshHosts: () => invoke<SshHost[]>("list_ssh_hosts"),
  sshConfigHosts: (chatId?: string | null) => invoke<SshConfigHost[]>("ssh_config_hosts", { chatId }),
  saveSshHost: (host: Partial<SshHost>, secret?: string) => invoke<string>("save_ssh_host", { host, secret }),
  deleteSshHost: (id: string) => invoke<void>("delete_ssh_host", { id }),
  testSshHost: (id: string) => invoke<string>("test_ssh_host", { id }),
  forgetHostKey: (id: string) => invoke<void>("forget_host_key", { id }),

  // ---- tools
  tools: () => invoke<ToolSummary[]>("list_tools"),

  // ---- files
  workspaceStatus: (chatId?: string | null) => invoke<WorkspaceStatus>("workspace_status", { chatId }),
  gitInit: (path: string) => invoke<WorkspaceStatus>("git_init", { path }),

  fsHome: () => invoke<string>("fs_home"),
  fsList: (path: string, showHidden = false) => invoke<FsEntry[]>("fs_list", { path, showHidden }),
  fsRead: (path: string) => invoke<FileContents>("fs_read", { path }),
  fsWrite: (args: { path: string; content: string; expectedHash?: string; chatId?: string }) =>
    invoke<FileContents>("fs_write", args),
  fsDelete: (path: string, chatId?: string, recursive?: boolean) =>
    invoke<string | null>("fs_delete", { path, chatId, recursive }),

  // ---- model execution graph
  graphList: () => invoke<GraphDoc[]>("graph_list"),
  graphSave: (args: { id?: string; projectId?: string | null; name: string; spec: GraphSpec }) =>
    invoke<GraphDoc>("graph_save", args),
  graphDelete: (id: string) => invoke<void>("graph_delete", { id }),
  graphBind: (chatId: string, graphId: string | null) => invoke<void>("graph_bind", { chatId, graphId }),
  graphBinding: (chatId: string) => invoke<string | null>("graph_binding", { chatId }),
  graphClearCache: (id: string, nodeId?: string) => invoke<GraphDoc>("graph_clear_cache", { id, nodeId }),
  graphSetArtifact: (id: string, nodeId: string, content: string) =>
    invoke<GraphDoc>("graph_set_artifact", { id, nodeId, content }),
  graphRun: (chatId: string, graphId: string, text: string, onEvent: Channel<AgentEvent>) =>
    invoke<string>("graph_run", { chatId, graphId, text, onEvent }),
  graphRefine: (
    chatId: string,
    graphId: string,
    nodeId: string,
    instruction: string,
    onEvent: Channel<AgentEvent>,
  ) => invoke<string>("graph_refine", { chatId, graphId, nodeId, instruction, onEvent }),
  graphPlan: (chatId: string, goal: string, customs?: { name: string; stages: string[] }[]) =>
    invoke<import("./meg").AutoPlan>("graph_plan", { chatId, goal, customs }),

  // ---- architecture analysis
  analysisGet: (chatId: string) => invoke<AnalysisSnapshot>("analysis_get", { chatId }),
  analysisScan: (
    chatId: string,
    enrich: boolean,
    model: string | null,
    scope: string | null,
    onEvent: Channel<AnalysisEvent>,
  ) => invoke<ArchDoc>("analysis_scan", { chatId, enrich, model, scope, onEvent }),
  analysisCancel: (chatId: string) => invoke<void>("analysis_cancel", { chatId }),

  // ---- change journal
  chatChanges: (chatId: string) => invoke<ChangedFile[]>("chat_changes", { chatId }),
  artifacts: (projectId?: string | null, limit?: number) =>
    invoke<Artifact[]>("list_artifacts", { projectId, limit }),
  changeDiff: (changeId: string) => invoke<ChangeDiff>("change_diff", { changeId }),
  revertChange: (changeId: string, force = false) =>
    invoke<RevertResult>("revert_change", { changeId, force }),
  revertRun: (runId: string, force = false) => invoke<RevertResult[]>("revert_run", { runId, force }),
  revertAll: (chatId: string, force = false) =>
    invoke<RevertResult[]>("revert_all_changes", { chatId, force }),
};

export { Channel };
