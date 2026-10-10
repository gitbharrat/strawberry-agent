//! SQLite persistence layer.
//!
//! Everything the app knows lives here: projects, chats, messages, tool calls,
//! runs and SSH hosts. The database is opened once in WAL mode and guarded by a
//! plain mutex — queries are single-digit microseconds on local storage, and no
//! caller ever holds the guard across an `.await`, so this never blocks the
//! async runtime.

use crate::error::{AppError, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub struct Db {
    conn: Mutex<Connection>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_id() -> String {
    // v7 is time-ordered, which keeps b-tree inserts sequential and makes ids
    // sort chronologically without a separate index.
    uuid::Uuid::now_v7().to_string()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  root_path   TEXT,
  color       TEXT,
  icon        TEXT,
  system_prompt TEXT,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  archived    INTEGER NOT NULL DEFAULT 0,
  sort_order  REAL NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS chats (
  id            TEXT PRIMARY KEY,
  project_id    TEXT REFERENCES projects(id) ON DELETE CASCADE,
  title         TEXT NOT NULL,
  model         TEXT,
  system_prompt TEXT,
  cwd           TEXT,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  archived      INTEGER NOT NULL DEFAULT 0,
  pinned        INTEGER NOT NULL DEFAULT 0,
  total_cost    REAL NOT NULL DEFAULT 0,
  message_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_chats_project ON chats(project_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS messages (
  id           TEXT PRIMARY KEY,
  chat_id      TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  seq          INTEGER NOT NULL,
  role         TEXT NOT NULL,
  content      TEXT NOT NULL DEFAULT '',
  reasoning    TEXT,
  tool_calls   TEXT,
  tool_call_id TEXT,
  model        TEXT,
  status       TEXT NOT NULL DEFAULT 'complete',
  error        TEXT,
  usage        TEXT,
  run_id       TEXT,
  created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, seq);

CREATE TABLE IF NOT EXISTS attachments (
  id         TEXT PRIMARY KEY,
  chat_id    TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  message_id TEXT,
  name       TEXT NOT NULL,
  mime       TEXT NOT NULL,
  size       INTEGER NOT NULL,
  path       TEXT NOT NULL,
  kind       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_id);

CREATE TABLE IF NOT EXISTS runs (
  id          TEXT PRIMARY KEY,
  chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  status      TEXT NOT NULL,
  model       TEXT,
  steps       INTEGER NOT NULL DEFAULT 0,
  tokens_in   INTEGER NOT NULL DEFAULT 0,
  tokens_out  INTEGER NOT NULL DEFAULT 0,
  cost        REAL NOT NULL DEFAULT 0,
  error       TEXT,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_runs_chat ON runs(chat_id, started_at DESC);

CREATE TABLE IF NOT EXISTS tool_calls (
  id          TEXT PRIMARY KEY,
  run_id      TEXT NOT NULL,
  chat_id     TEXT NOT NULL,
  message_id  TEXT,
  name        TEXT NOT NULL,
  args        TEXT NOT NULL DEFAULT '{}',
  result      TEXT,
  status      TEXT NOT NULL,
  error       TEXT,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_tool_calls_run ON tool_calls(run_id, started_at);

CREATE TABLE IF NOT EXISTS file_changes (
  id           TEXT PRIMARY KEY,
  chat_id      TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  run_id       TEXT,
  tool_call_id TEXT,
  path         TEXT NOT NULL,
  op           TEXT NOT NULL,
  origin       TEXT NOT NULL DEFAULT 'agent',
  before_path  TEXT,
  after_hash   TEXT,
  bytes_before INTEGER NOT NULL DEFAULT 0,
  bytes_after  INTEGER NOT NULL DEFAULT 0,
  reverted     INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_file_changes_chat ON file_changes(chat_id, created_at DESC);

CREATE TABLE IF NOT EXISTS ssh_hosts (
  id         TEXT PRIMARY KEY,
  label      TEXT NOT NULL,
  host       TEXT NOT NULL,
  port       INTEGER NOT NULL DEFAULT 22,
  username   TEXT NOT NULL,
  auth       TEXT NOT NULL,
  key_path   TEXT,
  created_at INTEGER NOT NULL,
  last_used  INTEGER
);

CREATE TABLE IF NOT EXISTS model_cache (
  id         TEXT PRIMARY KEY,
  payload    TEXT NOT NULL,
  fetched_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS graphs (
  id         TEXT PRIMARY KEY,
  project_id TEXT,
  name       TEXT NOT NULL,
  spec       TEXT NOT NULL,
  artifacts  TEXT NOT NULL DEFAULT '{}',
  updated_at INTEGER NOT NULL
);
"#;

impl Db {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("strawberry.db"))?;
        // WAL + NORMAL keeps writes off the read path and survives crashes;
        // together they are the difference between an instant UI and a stuttery
        // one when a long run is appending deltas.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
        conn.pragma_update(None, "cache_size", -32_000i64)?;
        conn.execute_batch(SCHEMA)?;
        let db = Self { conn: Mutex::new(conn) };
        db.recover_orphaned_runs()?;
        db.purge_abandoned_chats()?;
        Ok(db)
    }

    /// A run that was live when the process died can never resume, so mark it
    /// interrupted at startup rather than leaving a spinner in the UI forever.
    fn recover_orphaned_runs(&self) -> Result<()> {
        // Runs that died before writing a single assistant message would leave
        // a wordless gap in the transcript — remember them before the sweep.
        let silent: Vec<(String, String)> = {
            let c = self.conn.lock();
            let mut st = c.prepare(
                "SELECT id, chat_id FROM runs
                 WHERE status IN ('running','pending')
                   AND NOT EXISTS (SELECT 1 FROM messages m
                                   WHERE m.run_id = runs.id AND m.role = 'assistant')",
            )?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        {
            let c = self.conn.lock();
            c.execute(
                "UPDATE runs SET status='interrupted', finished_at=?1, error='Application closed during run'
                 WHERE status IN ('running','pending')",
                params![now_ms()],
            )?;
            c.execute(
                "UPDATE messages SET status='interrupted' WHERE status IN ('streaming','pending')",
                [],
            )?;
            c.execute(
                "UPDATE tool_calls SET status='interrupted', finished_at=?1
                 WHERE status IN ('running','pending','awaiting_approval')",
                params![now_ms()],
            )?;
        }
        for (run_id, chat_id) in silent {
            let _ = self.insert_message(&NewMessage {
                id: None,
                chat_id,
                role: "assistant".into(),
                content: "_This run was interrupted — the app closed before a reply was produced. Send the message again to retry._".into(),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                model: None,
                status: "interrupted".into(),
                error: None,
                usage: None,
                run_id: Some(run_id),
            });
        }
        Ok(())
    }

    /// Deletes untitled chats that never received a message and are at least an
    /// hour old. They are the residue of "New chat" pressed and abandoned;
    /// keeping them turns the rail into a graveyard. The hour of grace protects
    /// a chat someone opened moments before restarting the app.
    fn purge_abandoned_chats(&self) -> Result<()> {
        let cutoff = chrono::Utc::now().timestamp_millis() - 60 * 60 * 1000;
        let c = self.conn.lock();
        c.execute(
            "DELETE FROM chats
             WHERE title = 'New chat'
               AND created_at < ?1
               AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.chat_id = chats.id)
               AND NOT EXISTS (SELECT 1 FROM file_changes f WHERE f.chat_id = chats.id)",
            params![cutoff],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- meta

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let c = self.conn.lock();
        Ok(c.query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ projects

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT p.id,p.name,p.root_path,p.color,p.icon,p.system_prompt,p.created_at,p.updated_at,
                    p.archived,p.sort_order,
                    (SELECT COUNT(*) FROM chats ch WHERE ch.project_id=p.id AND ch.archived=0)
             FROM projects p WHERE p.archived=0 ORDER BY p.sort_order, p.created_at",
        )?;
        let rows = st.query_map([], Project::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_project(&self, name: &str, root: Option<&str>) -> Result<Project> {
        let id = new_id();
        let ts = now_ms();
        {
            let c = self.conn.lock();
            c.execute(
                "INSERT INTO projects(id,name,root_path,created_at,updated_at,sort_order)
                 VALUES(?1,?2,?3,?4,?4,?4)",
                params![id, name, root, ts],
            )?;
        }
        self.get_project(&id)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT p.id,p.name,p.root_path,p.color,p.icon,p.system_prompt,p.created_at,p.updated_at,
                    p.archived,p.sort_order,
                    (SELECT COUNT(*) FROM chats ch WHERE ch.project_id=p.id AND ch.archived=0)
             FROM projects p WHERE p.id=?1",
            params![id],
            Project::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("project {id}")))
    }

    pub fn update_project(&self, p: &ProjectPatch) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE projects SET
               name = COALESCE(?2, name),
               root_path = COALESCE(?3, root_path),
               color = COALESCE(?4, color),
               system_prompt = COALESCE(?5, system_prompt),
               archived = COALESCE(?6, archived),
               updated_at = ?7
             WHERE id = ?1",
            params![p.id, p.name, p.root_path, p.color, p.system_prompt, p.archived, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute("DELETE FROM projects WHERE id=?1", params![id])?;
        Ok(())
    }

    // --------------------------------------------------------------- chats

    pub fn list_chats(&self, project_id: Option<&str>) -> Result<Vec<Chat>> {
        let c = self.conn.lock();
        let sql = "SELECT id,project_id,title,model,system_prompt,cwd,created_at,updated_at,
                          archived,pinned,total_cost,message_count
                   FROM chats WHERE archived=0 AND (?1 IS NULL OR project_id IS ?1)
                   ORDER BY pinned DESC, updated_at DESC";
        let mut st = c.prepare(sql)?;
        let rows = st.query_map(params![project_id], Chat::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_chat(
        &self,
        project_id: Option<&str>,
        title: &str,
        model: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<Chat> {
        let id = new_id();
        let ts = now_ms();
        {
            let c = self.conn.lock();
            c.execute(
                "INSERT INTO chats(id,project_id,title,model,cwd,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?6)",
                params![id, project_id, title, model, cwd, ts],
            )?;
        }
        self.get_chat(&id)
    }

    pub fn get_chat(&self, id: &str) -> Result<Chat> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT id,project_id,title,model,system_prompt,cwd,created_at,updated_at,
                    archived,pinned,total_cost,message_count
             FROM chats WHERE id=?1",
            params![id],
            Chat::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("chat {id}")))
    }

    pub fn update_chat(&self, p: &ChatPatch) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE chats SET
               title = COALESCE(?2, title),
               model = COALESCE(?3, model),
               system_prompt = COALESCE(?4, system_prompt),
               cwd = COALESCE(?5, cwd),
               project_id = COALESCE(?6, project_id),
               archived = COALESCE(?7, archived),
               pinned = COALESCE(?8, pinned),
               updated_at = ?9
             WHERE id = ?1",
            params![
                p.id,
                p.title,
                p.model,
                p.system_prompt,
                p.cwd,
                p.project_id,
                p.archived,
                p.pinned,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn delete_chat(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute("DELETE FROM chats WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn touch_chat(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE chats SET updated_at=?2,
                message_count=(SELECT COUNT(*) FROM messages WHERE chat_id=?1)
             WHERE id=?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ messages

    pub fn list_messages(&self, chat_id: &str) -> Result<Vec<Message>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id,chat_id,seq,role,content,reasoning,tool_calls,tool_call_id,model,
                    status,error,usage,run_id,created_at
             FROM messages WHERE chat_id=?1 ORDER BY seq",
        )?;
        let rows = st.query_map(params![chat_id], Message::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn next_seq(c: &Connection, chat_id: &str) -> rusqlite::Result<i64> {
        c.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM messages WHERE chat_id=?1", params![chat_id], |r| {
            r.get(0)
        })
    }

    /// Removes every message strictly before `seq`. Compaction's pruning half.
    pub fn delete_messages_before(&self, chat_id: &str, seq: i64) -> Result<usize> {
        let c = self.conn.lock();
        let n = c.execute("DELETE FROM messages WHERE chat_id = ?1 AND seq < ?2", params![chat_id, seq])?;
        Ok(n)
    }

    /// Inserts a message at an explicit sequence number — compaction writes
    /// its summary into the hole the pruned history leaves behind.
    pub fn insert_message_at(&self, chat_id: &str, seq: i64, role: &str, content: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO messages(id,chat_id,seq,role,content,status,created_at)
             VALUES(?1,?2,?3,?4,?5,'complete',?6)",
            params![new_id(), chat_id, seq, role, content, now_ms()],
        )?;
        Ok(())
    }

    pub fn insert_message(&self, m: &NewMessage) -> Result<Message> {
        let id = m.id.clone().unwrap_or_else(new_id);
        let ts = now_ms();
        {
            let c = self.conn.lock();
            let seq = Self::next_seq(&c, &m.chat_id)?;
            c.execute(
                "INSERT INTO messages(id,chat_id,seq,role,content,reasoning,tool_calls,tool_call_id,
                                      model,status,error,usage,run_id,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    id,
                    m.chat_id,
                    seq,
                    m.role,
                    m.content,
                    m.reasoning,
                    m.tool_calls,
                    m.tool_call_id,
                    m.model,
                    m.status,
                    m.error,
                    m.usage,
                    m.run_id,
                    ts
                ],
            )?;
        }
        self.touch_chat(&m.chat_id)?;
        self.get_message(&id)
    }

    pub fn get_message(&self, id: &str) -> Result<Message> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT id,chat_id,seq,role,content,reasoning,tool_calls,tool_call_id,model,
                    status,error,usage,run_id,created_at
             FROM messages WHERE id=?1",
            params![id],
            Message::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("message {id}")))
    }

    /// Rewrites a message's content in place — the chat-side design editor's
    /// write path. Role, seq and provenance stay untouched.
    pub fn update_message_content(&self, id: &str, content: &str) -> Result<Message> {
        {
            let c = self.conn.lock();
            let n = c.execute("UPDATE messages SET content=?2 WHERE id=?1", params![id, content])?;
            if n == 0 {
                return Err(AppError::NotFound(format!("message {id}")));
            }
        }
        self.get_message(id)
    }

    /// Removes a message and every message after it — used by "edit and resend"
    /// and by retry, which both need to rewind the conversation.
    pub fn truncate_from(&self, chat_id: &str, seq: i64) -> Result<()> {
        let c = self.conn.lock();
        c.execute("DELETE FROM messages WHERE chat_id=?1 AND seq>=?2", params![chat_id, seq])?;
        Ok(())
    }

    pub fn delete_message(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute("DELETE FROM messages WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Distinct paths a chat has written to, for reporting.
    pub fn chat_file_changes_summary(&self, chat_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut st =
            conn.prepare("SELECT DISTINCT path FROM file_changes WHERE chat_id = ?1 ORDER BY created_at")?;
        let rows = st.query_map(params![chat_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Repoints chats that still carry a superseded default model. Chats whose
    /// model the user picked deliberately are left untouched.
    pub fn remap_default_model(&self, from: &[&str], to: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let mut moved = 0;
        for old in from {
            moved += conn.execute("UPDATE chats SET model = ?2 WHERE model = ?1", params![old, to])?;
        }
        Ok(moved)
    }

    // --------------------------------------------------------- attachments

    pub fn insert_attachment(&self, a: &Attachment) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO attachments(id,chat_id,message_id,name,mime,size,path,kind,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![a.id, a.chat_id, a.message_id, a.name, a.mime, a.size, a.path, a.kind, a.created_at],
        )?;
        Ok(())
    }

    pub fn attach_to_message(&self, attachment_ids: &[String], message_id: &str) -> Result<()> {
        let c = self.conn.lock();
        for id in attachment_ids {
            c.execute("UPDATE attachments SET message_id=?2 WHERE id=?1", params![id, message_id])?;
        }
        Ok(())
    }

    pub fn list_attachments(&self, chat_id: &str) -> Result<Vec<Attachment>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id,chat_id,message_id,name,mime,size,path,kind,created_at
             FROM attachments WHERE chat_id=?1 ORDER BY created_at",
        )?;
        let rows = st.query_map(params![chat_id], Attachment::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn attachments_for_message(&self, message_id: &str) -> Result<Vec<Attachment>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id,chat_id,message_id,name,mime,size,path,kind,created_at
             FROM attachments WHERE message_id=?1 ORDER BY created_at",
        )?;
        let rows = st.query_map(params![message_id], Attachment::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---------------------------------------------------------------- runs

    pub fn create_run(&self, id: &str, chat_id: &str, model: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO runs(id,chat_id,status,model,started_at) VALUES(?1,?2,'running',?3,?4)",
            params![id, chat_id, model, now_ms()],
        )?;
        Ok(())
    }

    pub fn finish_run(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE runs SET status=?2, error=?3, finished_at=?4 WHERE id=?1",
            params![id, status, error, now_ms()],
        )?;
        Ok(())
    }

    pub fn bump_run_usage(&self, id: &str, tin: i64, tout: i64, cost: f64, steps: i64) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE runs SET tokens_in=tokens_in+?2, tokens_out=tokens_out+?3,
                             cost=cost+?4, steps=steps+?5 WHERE id=?1",
            params![id, tin, tout, cost, steps],
        )?;
        c.execute(
            "UPDATE chats SET total_cost=total_cost+?2 WHERE id=(SELECT chat_id FROM runs WHERE id=?1)",
            params![id, cost],
        )?;
        Ok(())
    }

    pub fn active_runs(&self) -> Result<Vec<Run>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id,chat_id,status,model,steps,tokens_in,tokens_out,cost,error,started_at,finished_at
             FROM runs WHERE status IN ('running','pending') ORDER BY started_at",
        )?;
        let rows = st.query_map([], Run::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---------------------------------------------------------- tool calls

    pub fn insert_tool_call(&self, t: &ToolCallRecord) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO tool_calls(id,run_id,chat_id,message_id,name,args,status,started_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET args=excluded.args, status=excluded.status",
            params![t.id, t.run_id, t.chat_id, t.message_id, t.name, t.args, t.status, t.started_at],
        )?;
        Ok(())
    }

    pub fn finish_tool_call(
        &self,
        id: &str,
        status: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE tool_calls SET status=?2, result=?3, error=?4, finished_at=?5 WHERE id=?1",
            params![id, status, result, error, now_ms()],
        )?;
        Ok(())
    }

    pub fn list_tool_calls(&self, chat_id: &str) -> Result<Vec<ToolCallRecord>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id,run_id,chat_id,message_id,name,args,result,status,error,started_at,finished_at
             FROM tool_calls WHERE chat_id=?1 ORDER BY started_at",
        )?;
        let rows = st.query_map(params![chat_id], ToolCallRecord::from_row_full)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -------------------------------------------------------- file changes

    pub fn record_file_change(&self, c: &FileChange) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO file_changes(id,chat_id,run_id,tool_call_id,path,op,origin,
                                      before_path,after_hash,bytes_before,bytes_after,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                c.id,
                c.chat_id,
                c.run_id,
                c.tool_call_id,
                c.path,
                c.op,
                c.origin,
                c.before_path,
                c.after_hash,
                c.bytes_before,
                c.bytes_after,
                c.created_at
            ],
        )?;
        Ok(())
    }

    pub fn list_file_changes(&self, chat_id: &str) -> Result<Vec<FileChange>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT id,chat_id,run_id,tool_call_id,path,op,origin,before_path,after_hash,
                    bytes_before,bytes_after,reverted,created_at
             FROM file_changes WHERE chat_id=?1 ORDER BY created_at DESC",
        )?;
        let rows = st.query_map(params![chat_id], FileChange::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether a run wrote through the file tools. Remote-workspace edits are
    /// never journalled (the undo system snapshots this machine's disk), so
    /// the tool ledger is the "did anything change" signal there.
    pub fn run_wrote_files(&self, run_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tool_calls
             WHERE run_id=?1 AND name IN ('write_file','edit_file') AND status='complete'",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Unreverted changes belonging to one run, newest first — the order a
    /// revert must apply them in so multi-edited files land on their original
    /// contents.
    pub fn run_file_changes(&self, run_id: &str) -> Result<Vec<FileChange>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT id,chat_id,run_id,tool_call_id,path,op,origin,before_path,after_hash,
                    bytes_before,bytes_after,reverted,created_at
             FROM file_changes WHERE run_id=?1 AND reverted=0 ORDER BY created_at DESC",
        )?;
        let rows = st.query_map(params![run_id], FileChange::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_file_change(&self, id: &str) -> Result<FileChange> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT id,chat_id,run_id,tool_call_id,path,op,origin,before_path,after_hash,
                    bytes_before,bytes_after,reverted,created_at
             FROM file_changes WHERE id=?1",
            params![id],
            FileChange::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("change {id}")))
    }

    pub fn mark_reverted(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("UPDATE file_changes SET reverted=1 WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Files the agent has created that still exist — the "artifacts" a chat
    /// produced, as opposed to files it merely edited.
    pub fn list_artifacts(&self, project_id: Option<&str>, limit: i64) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT fc.id, fc.path, fc.chat_id, c.title, MAX(fc.created_at) AS at
             FROM file_changes fc
             JOIN chats c ON c.id = fc.chat_id
             WHERE fc.op = 'create' AND fc.reverted = 0
               AND (?1 IS NULL OR c.project_id IS ?1)
             GROUP BY fc.path
             ORDER BY at DESC
             LIMIT ?2",
        )?;
        let rows = st.query_map(params![project_id, limit], |r| {
            let path: String = r.get(1)?;
            Ok(Artifact {
                change_id: r.get(0)?,
                name: std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone()),
                path,
                chat_id: r.get(2)?,
                chat_title: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----------------------------------------------------------- ssh hosts

    pub fn list_ssh_hosts(&self) -> Result<Vec<SshHost>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id,label,host,port,username,auth,key_path,created_at,last_used
             FROM ssh_hosts ORDER BY last_used DESC NULLS LAST, label",
        )?;
        let rows = st.query_map([], SshHost::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_ssh_host(&self, id: &str) -> Result<SshHost> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT id,label,host,port,username,auth,key_path,created_at,last_used
             FROM ssh_hosts WHERE id=?1",
            params![id],
            SshHost::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("ssh host {id}")))
    }

    pub fn upsert_ssh_host(&self, h: &SshHost) -> Result<String> {
        let id = if h.id.is_empty() { new_id() } else { h.id.clone() };
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO ssh_hosts(id,label,host,port,username,auth,key_path,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET label=excluded.label, host=excluded.host,
               port=excluded.port, username=excluded.username, auth=excluded.auth,
               key_path=excluded.key_path",
            params![id, h.label, h.host, h.port, h.username, h.auth, h.key_path, now_ms()],
        )?;
        Ok(id)
    }

    pub fn delete_ssh_host(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute("DELETE FROM ssh_hosts WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn touch_ssh_host(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute("UPDATE ssh_hosts SET last_used=?2 WHERE id=?1", params![id, now_ms()])?;
        Ok(())
    }

    // --------------------------------------------------------- model cache

    pub fn cache_models(&self, payload: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO model_cache(id,payload,fetched_at) VALUES('openrouter',?1,?2)
             ON CONFLICT(id) DO UPDATE SET payload=excluded.payload, fetched_at=excluded.fetched_at",
            params![payload, now_ms()],
        )?;
        Ok(())
    }

    pub fn clear_model_cache(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM model_cache", [])?;
        Ok(())
    }

    pub fn cached_models(&self) -> Result<Option<(String, i64)>> {
        let c = self.conn.lock();
        Ok(c.query_row("SELECT payload, fetched_at FROM model_cache WHERE id='openrouter'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?)
    }

    // ---------------------------------------------------------------- graphs

    pub fn list_graphs(&self) -> Result<Vec<GraphRow>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT id, project_id, name, spec, artifacts, updated_at FROM graphs ORDER BY updated_at DESC",
        )?;
        let rows = st.query_map([], GraphRow::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_graph(&self, id: &str) -> Result<GraphRow> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT id, project_id, name, spec, artifacts, updated_at FROM graphs WHERE id=?1",
            params![id],
            GraphRow::from_row,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("graph {id}")))
    }

    /// Upserts a graph's definition. The artifact cache is deliberately left
    /// alone: editing a prompt must not throw away every other node's output.
    pub fn save_graph(&self, id: &str, project_id: Option<&str>, name: &str, spec: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO graphs(id, project_id, name, spec, artifacts, updated_at)
             VALUES(?1, ?2, ?3, ?4, '{}', ?5)
             ON CONFLICT(id) DO UPDATE SET
               project_id=excluded.project_id, name=excluded.name,
               spec=excluded.spec, updated_at=excluded.updated_at",
            params![id, project_id, name, spec, now_ms()],
        )?;
        Ok(())
    }

    pub fn set_graph_artifacts(&self, id: &str, artifacts: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE graphs SET artifacts=?2, updated_at=?3 WHERE id=?1",
            params![id, artifacts, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_graph(&self, id: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute("DELETE FROM graphs WHERE id=?1", params![id])?;
        Ok(())
    }

    // ------------------------------------------------------------ full-text

    /// Substring search across chat titles and message bodies. SQLite's LIKE on
    /// a few thousand rows is faster than maintaining an FTS index we'd have to
    /// keep in sync during streaming.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT m.chat_id, c.title, m.id, m.role, substr(m.content, 1, 240), m.created_at
             FROM messages m JOIN chats c ON c.id = m.chat_id
             WHERE m.content LIKE ?1 ESCAPE '\\' AND c.archived = 0
             ORDER BY m.created_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![like, limit], |r| {
            Ok(SearchHit {
                chat_id: r.get(0)?,
                chat_title: r.get(1)?,
                message_id: r.get(2)?,
                role: r.get(3)?,
                snippet: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

// ------------------------------------------------------------------ models

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub system_prompt: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
    pub sort_order: f64,
    pub chat_count: i64,
}

impl Project {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            root_path: r.get(2)?,
            color: r.get(3)?,
            icon: r.get(4)?,
            system_prompt: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
            archived: r.get::<_, i64>(8)? != 0,
            sort_order: r.get(9)?,
            chat_count: r.get(10)?,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPatch {
    pub id: String,
    pub name: Option<String>,
    pub root_path: Option<String>,
    pub color: Option<String>,
    pub system_prompt: Option<String>,
    pub archived: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub cwd: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
    pub pinned: bool,
    pub total_cost: f64,
    pub message_count: i64,
}

impl Chat {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            project_id: r.get(1)?,
            title: r.get(2)?,
            model: r.get(3)?,
            system_prompt: r.get(4)?,
            cwd: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
            archived: r.get::<_, i64>(8)? != 0,
            pinned: r.get::<_, i64>(9)? != 0,
            total_cost: r.get(10)?,
            message_count: r.get(11)?,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPatch {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub archived: Option<i64>,
    pub pinned: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub chat_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub usage: Option<String>,
    pub run_id: Option<String>,
    pub created_at: i64,
}

impl Message {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            seq: r.get(2)?,
            role: r.get(3)?,
            content: r.get(4)?,
            reasoning: r.get(5)?,
            tool_calls: r.get(6)?,
            tool_call_id: r.get(7)?,
            model: r.get(8)?,
            status: r.get(9)?,
            error: r.get(10)?,
            usage: r.get(11)?,
            run_id: r.get(12)?,
            created_at: r.get(13)?,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewMessage {
    pub id: Option<String>,
    pub chat_id: String,
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub usage: Option<String>,
    pub run_id: Option<String>,
}

/// A stored Model Execution Graph. `spec` and `artifacts` are JSON blobs the
/// executor and frontend both parse; the columns stay separate so a spec save
/// never clobbers the artifact cache and vice versa.
#[derive(Debug, Clone)]
pub struct GraphRow {
    pub id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub spec: String,
    pub artifacts: String,
    pub updated_at: i64,
}

impl GraphRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            project_id: r.get(1)?,
            name: r.get(2)?,
            spec: r.get(3)?,
            artifacts: r.get(4)?,
            updated_at: r.get(5)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub chat_id: String,
    pub message_id: Option<String>,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub path: String,
    /// `image` | `text` | `binary` — decides how it is fed to the model.
    pub kind: String,
    pub created_at: i64,
}

impl Attachment {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            message_id: r.get(2)?,
            name: r.get(3)?,
            mime: r.get(4)?,
            size: r.get(5)?,
            path: r.get(6)?,
            kind: r.get(7)?,
            created_at: r.get(8)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub chat_id: String,
    pub status: String,
    pub model: Option<String>,
    pub steps: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost: f64,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl Run {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            status: r.get(2)?,
            model: r.get(3)?,
            steps: r.get(4)?,
            tokens_in: r.get(5)?,
            tokens_out: r.get(6)?,
            cost: r.get(7)?,
            error: r.get(8)?,
            started_at: r.get(9)?,
            finished_at: r.get(10)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub id: String,
    pub run_id: String,
    pub chat_id: String,
    pub message_id: Option<String>,
    pub name: String,
    pub args: String,
    #[serde(default)]
    pub result: Option<String>,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
}

impl ToolCallRecord {
    fn from_row_full(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            run_id: r.get(1)?,
            chat_id: r.get(2)?,
            message_id: r.get(3)?,
            name: r.get(4)?,
            args: r.get(5)?,
            result: r.get(6)?,
            status: r.get(7)?,
            error: r.get(8)?,
            started_at: r.get(9)?,
            finished_at: r.get(10)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHost {
    #[serde(default)]
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    /// `password` | `key` | `agent`
    pub auth: String,
    pub key_path: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_used: Option<i64>,
}

impl SshHost {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            label: r.get(1)?,
            host: r.get(2)?,
            port: r.get(3)?,
            username: r.get(4)?,
            auth: r.get(5)?,
            key_path: r.get(6)?,
            created_at: r.get(7)?,
            last_used: r.get(8)?,
        })
    }
}

/// One recorded mutation of a file, with enough information to undo it.
///
/// `before_path` points at a snapshot of the previous contents, and
/// `after_hash` records what the file looked like immediately after the change
/// — so a revert can tell whether someone has edited the file since.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub id: String,
    pub chat_id: String,
    pub run_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub path: String,
    /// `create` | `modify` | `delete`
    pub op: String,
    /// `agent` | `manual`
    pub origin: String,
    pub before_path: Option<String>,
    pub after_hash: Option<String>,
    pub bytes_before: i64,
    pub bytes_after: i64,
    #[serde(default)]
    pub reverted: bool,
    pub created_at: i64,
}

impl FileChange {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            run_id: r.get(2)?,
            tool_call_id: r.get(3)?,
            path: r.get(4)?,
            op: r.get(5)?,
            origin: r.get(6)?,
            before_path: r.get(7)?,
            after_hash: r.get(8)?,
            bytes_before: r.get(9)?,
            bytes_after: r.get(10)?,
            reverted: r.get::<_, i64>(11)? != 0,
            created_at: r.get(12)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub change_id: String,
    pub path: String,
    pub name: String,
    pub chat_id: String,
    pub chat_title: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub chat_id: String,
    pub chat_title: String,
    pub message_id: String,
    pub role: String,
    pub snippet: String,
    pub created_at: i64,
}

/// Directory that holds the database and uploaded attachments.
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("StrawberryAgent")
}
