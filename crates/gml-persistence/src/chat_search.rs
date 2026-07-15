//! Indexed chat search over dialog metadata and player-facing transcript text.
//!
//! The source of truth remains `dialog_chats`. This module owns a derived FTS5
//! index that is rebuilt for existing databases and updated transactionally by
//! [`crate::DialogStore`]. Hidden reasoning, tool payloads, and other GM-only
//! data are deliberately excluded from the searchable document.

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::Value;

use crate::StoreError;

const SEARCH_SCHEMA_VERSION: i64 = 1;
const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 50;
const MAX_QUERY_CHARS: usize = 160;

/// Which chat field a query is restricted to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatSearchScope {
    /// Search metadata and player-facing messages together.
    #[default]
    All,
    Title,
    World,
    Story,
    Character,
    Messages,
}

/// Result ordering for [`ChatSearchQuery`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatSearchSort {
    /// FTS relevance, with most recently updated chats as the tie-breaker.
    #[default]
    Relevance,
    /// Most recently updated chats first.
    Updated,
}

/// Typed persistence query used by the HTTP/application layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSearchQuery {
    pub text: String,
    pub scope: ChatSearchScope,
    pub world_id: Option<String>,
    pub story_id: Option<String>,
    pub character_id: Option<String>,
    pub kind: Option<String>,
    pub updated_after: Option<String>,
    pub has_messages: Option<bool>,
    pub sort: ChatSearchSort,
    pub limit: usize,
    pub offset: usize,
}

impl Default for ChatSearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            scope: ChatSearchScope::All,
            world_id: None,
            story_id: None,
            character_id: None,
            kind: None,
            updated_after: None,
            has_messages: None,
            sort: ChatSearchSort::Relevance,
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

/// One chat returned by [`crate::DialogStore::search_chats`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChatSearchHit {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub turn_count: i64,
    pub story_id: String,
    pub story_title: String,
    pub world_id: String,
    pub world_title: String,
    pub character_id: String,
    pub character_name: String,
    pub kind: String,
    pub created_at: String,
    pub updated_at: String,
    pub active: bool,
    /// Stable wire labels: `title`, `world`, `story`, `character`, `messages`.
    pub matched_fields: Vec<String>,
    /// A plain-text excerpt from the first matching player-facing message.
    pub snippet: String,
    /// Higher is better within one FTS result set. The value is a relative
    /// SQLite BM25 tie-breaker, not a cross-entity/global-search score. It is
    /// zero when no full-text query was supplied.
    pub score: f64,
}

/// A bounded page of chat search hits.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatSearchPage {
    pub hits: Vec<ChatSearchHit>,
    pub total: usize,
    pub has_more: bool,
}

/// Metadata extracted from the persisted session snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChatMetadata {
    pub story_id: String,
    pub story_title: String,
    pub world_id: String,
    pub world_title: String,
    pub character_id: String,
    pub character_name: String,
    pub kind: String,
    visible_messages: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ChatSearchDocument {
    guest_id: String,
    chat_id: String,
    title: String,
    preview: String,
    turn_count: i64,
    metadata: ChatMetadata,
    created_at: String,
    updated_at: String,
}

impl ChatSearchDocument {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_payload(
        guest_id: &str,
        chat_id: &str,
        title: &str,
        preview: &str,
        turn_count: i64,
        payload: &str,
        created_at: &str,
        updated_at: &str,
    ) -> Self {
        Self {
            guest_id: guest_id.to_string(),
            chat_id: chat_id.to_string(),
            title: title.to_string(),
            preview: preview.to_string(),
            turn_count: turn_count.max(0),
            metadata: metadata_from_payload(payload),
            created_at: created_at.to_string(),
            updated_at: updated_at.to_string(),
        }
    }
}

/// Create or migrate the derived search schema, then backfill every chat that
/// predates it. A small private schema marker makes future extractor changes
/// explicitly rebuildable without touching the dialog payload schema version.
pub(crate) fn initialize(con: &Connection) -> Result<(), StoreError> {
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS dialog_chat_search_meta (
             key TEXT PRIMARY KEY,
             value INTEGER NOT NULL
         );",
    )?;
    let current: Option<i64> = con
        .query_row(
            "SELECT value FROM dialog_chat_search_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let tx = con.unchecked_transaction()?;
    if current != Some(SEARCH_SCHEMA_VERSION) {
        tx.execute_batch(
            "DROP TABLE IF EXISTS dialog_chat_search_fts;
             DROP TABLE IF EXISTS dialog_chat_search_docs;",
        )?;
    }
    create_search_tables(&tx)?;

    let rows = missing_documents(&tx, current != Some(SEARCH_SCHEMA_VERSION))?;
    for document in rows {
        upsert_document(&tx, &document)?;
    }
    tx.execute(
        "INSERT INTO dialog_chat_search_meta (key, value)
         VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [SEARCH_SCHEMA_VERSION],
    )?;
    tx.commit()?;
    Ok(())
}

fn create_search_tables(con: &Connection) -> Result<(), StoreError> {
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS dialog_chat_search_docs (
             id INTEGER PRIMARY KEY,
             guest_id TEXT NOT NULL,
             chat_id TEXT NOT NULL,
             title TEXT NOT NULL,
             preview TEXT NOT NULL,
             turn_count INTEGER NOT NULL DEFAULT 0,
             story_id TEXT NOT NULL DEFAULT '',
             story_title TEXT NOT NULL DEFAULT '',
             world_id TEXT NOT NULL DEFAULT '',
             world_title TEXT NOT NULL DEFAULT '',
             character_id TEXT NOT NULL DEFAULT '',
             character_name TEXT NOT NULL DEFAULT '',
             kind TEXT NOT NULL DEFAULT 'chat',
             visible_messages TEXT NOT NULL DEFAULT '',
             has_messages INTEGER NOT NULL DEFAULT 0,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             UNIQUE (guest_id, chat_id)
         );
         CREATE INDEX IF NOT EXISTS idx_dialog_chat_search_guest_updated
             ON dialog_chat_search_docs (guest_id, updated_at DESC, chat_id DESC);
         CREATE INDEX IF NOT EXISTS idx_dialog_chat_search_world
             ON dialog_chat_search_docs (guest_id, world_id);
         CREATE INDEX IF NOT EXISTS idx_dialog_chat_search_story
             ON dialog_chat_search_docs (guest_id, story_id);
         CREATE INDEX IF NOT EXISTS idx_dialog_chat_search_character
             ON dialog_chat_search_docs (guest_id, character_id);
         CREATE VIRTUAL TABLE IF NOT EXISTS dialog_chat_search_fts USING fts5(
             title, world, story, character, messages,
             tokenize = 'unicode61 remove_diacritics 2'
         );",
    )?;
    Ok(())
}

fn missing_documents(
    con: &Connection,
    full_rebuild: bool,
) -> Result<Vec<ChatSearchDocument>, StoreError> {
    let predicate = if full_rebuild {
        String::new()
    } else {
        " WHERE NOT EXISTS (
              SELECT 1 FROM dialog_chat_search_docs d
              WHERE d.guest_id = c.guest_id AND d.chat_id = c.chat_id
          )"
        .to_string()
    };
    let sql = format!(
        "SELECT c.guest_id, c.chat_id, c.title, c.preview, c.turn_count,
                c.payload, c.created_at, c.updated_at
         FROM dialog_chats c{predicate}"
    );
    let mut stmt = con.prepare(&sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ChatSearchDocument::from_payload(
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                &row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                &row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                &row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                &row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Upsert both the filter metadata and FTS row. Callers invoke this through the
/// same SQLite transaction as the authoritative `dialog_chats` write.
pub(crate) fn upsert_document(
    con: &Connection,
    document: &ChatSearchDocument,
) -> Result<(), StoreError> {
    let existing_id: Option<i64> = con
        .query_row(
            "SELECT id FROM dialog_chat_search_docs
             WHERE guest_id = ?1 AND chat_id = ?2",
            params![document.guest_id, document.chat_id],
            |row| row.get(0),
        )
        .optional()?;
    let has_messages = i64::from(!document.metadata.visible_messages.trim().is_empty());

    let row_id = if let Some(id) = existing_id {
        con.execute(
            "UPDATE dialog_chat_search_docs SET
                 title = ?3, preview = ?4, turn_count = ?5,
                 story_id = ?6, story_title = ?7,
                 world_id = ?8, world_title = ?9,
                 character_id = ?10, character_name = ?11,
                 kind = ?12, visible_messages = ?13, has_messages = ?14,
                 created_at = ?15, updated_at = ?16
             WHERE guest_id = ?1 AND chat_id = ?2",
            params![
                document.guest_id,
                document.chat_id,
                document.title,
                document.preview,
                document.turn_count,
                document.metadata.story_id,
                document.metadata.story_title,
                document.metadata.world_id,
                document.metadata.world_title,
                document.metadata.character_id,
                document.metadata.character_name,
                document.metadata.kind,
                document.metadata.visible_messages,
                has_messages,
                document.created_at,
                document.updated_at,
            ],
        )?;
        con.execute("DELETE FROM dialog_chat_search_fts WHERE rowid = ?1", [id])?;
        id
    } else {
        con.execute(
            "INSERT INTO dialog_chat_search_docs (
                 guest_id, chat_id, title, preview, turn_count,
                 story_id, story_title, world_id, world_title,
                 character_id, character_name, kind, visible_messages,
                 has_messages, created_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                 ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
             )",
            params![
                document.guest_id,
                document.chat_id,
                document.title,
                document.preview,
                document.turn_count,
                document.metadata.story_id,
                document.metadata.story_title,
                document.metadata.world_id,
                document.metadata.world_title,
                document.metadata.character_id,
                document.metadata.character_name,
                document.metadata.kind,
                document.metadata.visible_messages,
                has_messages,
                document.created_at,
                document.updated_at,
            ],
        )?;
        con.last_insert_rowid()
    };

    con.execute(
        "INSERT INTO dialog_chat_search_fts (
             rowid, title, world, story, character, messages
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            row_id,
            normalize_for_search(&document.title),
            normalize_for_search(&document.metadata.world_title),
            normalize_for_search(&document.metadata.story_title),
            normalize_for_search(&document.metadata.character_name),
            normalize_for_search(&document.metadata.visible_messages),
        ],
    )?;
    Ok(())
}

/// Remove a search document transactionally with its source chat.
pub(crate) fn delete_document(
    con: &Connection,
    guest_id: &str,
    chat_id: &str,
) -> Result<(), StoreError> {
    let row_id: Option<i64> = con
        .query_row(
            "SELECT id FROM dialog_chat_search_docs
             WHERE guest_id = ?1 AND chat_id = ?2",
            params![guest_id, chat_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = row_id {
        con.execute("DELETE FROM dialog_chat_search_fts WHERE rowid = ?1", [id])?;
        con.execute("DELETE FROM dialog_chat_search_docs WHERE id = ?1", [id])?;
    }
    Ok(())
}

pub(crate) fn search(
    con: &Connection,
    guest_id: &str,
    query: &ChatSearchQuery,
) -> Result<ChatSearchPage, StoreError> {
    if query.text.chars().count() > MAX_QUERY_CHARS {
        return Err(StoreError::Payload(format!(
            "chat search query exceeds {MAX_QUERY_CHARS} characters"
        )));
    }
    let guest_id = guest_id.trim();
    if guest_id.is_empty() {
        return Err(StoreError::Other("guest_id is required".to_string()));
    }

    let tokens = search_tokens(&query.text);
    if !query.text.trim().is_empty() && tokens.is_empty() {
        return Ok(ChatSearchPage {
            hits: Vec::new(),
            total: 0,
            has_more: false,
        });
    }
    let fts = fts_expression(&tokens, query.scope);
    let uses_fts = fts.is_some();
    let mut conditions = Vec::new();
    let mut values = Vec::<SqlValue>::new();
    if let Some(expression) = fts {
        conditions.push("dialog_chat_search_fts MATCH ?".to_string());
        values.push(SqlValue::Text(expression));
    }
    conditions.push("d.guest_id = ?".to_string());
    values.push(SqlValue::Text(guest_id.to_string()));
    push_text_filter(
        &mut conditions,
        &mut values,
        "d.world_id",
        query.world_id.as_deref(),
    );
    push_text_filter(
        &mut conditions,
        &mut values,
        "d.story_id",
        query.story_id.as_deref(),
    );
    push_text_filter(
        &mut conditions,
        &mut values,
        "d.character_id",
        query.character_id.as_deref(),
    );
    push_text_filter(
        &mut conditions,
        &mut values,
        "d.kind",
        query.kind.as_deref(),
    );
    if let Some(updated_after) = query
        .updated_after
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        conditions.push("d.updated_at >= ?".to_string());
        values.push(SqlValue::Text(updated_after.to_string()));
    }
    if let Some(has_messages) = query.has_messages {
        conditions.push("d.has_messages = ?".to_string());
        values.push(SqlValue::Integer(i64::from(has_messages)));
    }

    let join = if uses_fts {
        "FROM dialog_chat_search_fts
         JOIN dialog_chat_search_docs d ON d.id = dialog_chat_search_fts.rowid"
    } else {
        "FROM dialog_chat_search_docs d"
    };
    let where_sql = format!("WHERE {}", conditions.join(" AND "));
    let count_sql = format!("SELECT COUNT(*) {join} {where_sql}");
    let total: i64 = con.query_row(&count_sql, params_from_iter(values.iter()), |row| {
        row.get(0)
    })?;

    let score_sql = if uses_fts {
        "-bm25(dialog_chat_search_fts, 8.0, 5.0, 5.0, 6.0, 1.0)"
    } else {
        "0.0"
    };
    let order_sql = if uses_fts && query.sort == ChatSearchSort::Relevance {
        "ORDER BY bm25(dialog_chat_search_fts, 8.0, 5.0, 5.0, 6.0, 1.0) ASC,
                  d.updated_at DESC, d.chat_id DESC"
    } else {
        "ORDER BY d.updated_at DESC, d.created_at DESC, d.chat_id DESC"
    };
    let limit = if query.limit == 0 {
        DEFAULT_LIMIT
    } else {
        query.limit.min(MAX_LIMIT)
    };
    let mut page_values = values;
    page_values.push(SqlValue::Integer(limit as i64));
    page_values.push(SqlValue::Integer(
        i64::try_from(query.offset).unwrap_or(i64::MAX),
    ));
    let sql = format!(
        "SELECT d.chat_id, d.title, d.preview, d.turn_count,
                d.story_id, d.story_title, d.world_id, d.world_title,
                d.character_id, d.character_name, d.kind,
                d.created_at, d.updated_at, d.visible_messages, {score_sql}
         {join} {where_sql} {order_sql} LIMIT ? OFFSET ?"
    );
    let active_id: String = con
        .query_row(
            "SELECT active_chat_id FROM guest_dialog_state WHERE guest_id = ?1",
            [guest_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .unwrap_or_default();
    let mut stmt = con.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(page_values.iter()), |row| {
        Ok(RawHit {
            id: row.get(0)?,
            title: row.get(1)?,
            preview: row.get(2)?,
            turn_count: row.get::<_, i64>(3)?.max(0),
            story_id: row.get(4)?,
            story_title: row.get(5)?,
            world_id: row.get(6)?,
            world_title: row.get(7)?,
            character_id: row.get(8)?,
            character_name: row.get(9)?,
            kind: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            visible_messages: row.get(13)?,
            score: row.get(14)?,
        })
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let row = row?;
        hits.push(row.into_hit(&active_id, &tokens, query.scope));
    }
    let total = usize::try_from(total.max(0)).unwrap_or(usize::MAX);
    Ok(ChatSearchPage {
        has_more: query.offset.saturating_add(hits.len()) < total,
        hits,
        total,
    })
}

fn push_text_filter(
    conditions: &mut Vec<String>,
    values: &mut Vec<SqlValue>,
    column: &str,
    value: Option<&str>,
) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        conditions.push(format!("{column} = ?"));
        values.push(SqlValue::Text(value.to_string()));
    }
}

struct RawHit {
    id: String,
    title: String,
    preview: String,
    turn_count: i64,
    story_id: String,
    story_title: String,
    world_id: String,
    world_title: String,
    character_id: String,
    character_name: String,
    kind: String,
    created_at: String,
    updated_at: String,
    visible_messages: String,
    score: f64,
}

impl RawHit {
    fn into_hit(self, active_id: &str, tokens: &[String], scope: ChatSearchScope) -> ChatSearchHit {
        let fields = [
            ("title", ChatSearchScope::Title, self.title.as_str()),
            ("world", ChatSearchScope::World, self.world_title.as_str()),
            ("story", ChatSearchScope::Story, self.story_title.as_str()),
            (
                "character",
                ChatSearchScope::Character,
                self.character_name.as_str(),
            ),
            (
                "messages",
                ChatSearchScope::Messages,
                self.visible_messages.as_str(),
            ),
        ];
        let matched_fields = if tokens.is_empty() {
            Vec::new()
        } else {
            fields
                .into_iter()
                .filter(|(_, field_scope, text)| {
                    (scope == ChatSearchScope::All || scope == *field_scope)
                        && field_matches_any(text, tokens)
                })
                .map(|(name, _, _)| name.to_string())
                .collect()
        };
        let snippet = if matched_fields.iter().any(|field| field == "messages") {
            message_snippet(&self.visible_messages, tokens)
        } else {
            String::new()
        };
        ChatSearchHit {
            active: self.id == active_id,
            id: self.id,
            title: self.title,
            preview: self.preview,
            turn_count: self.turn_count,
            story_id: self.story_id,
            story_title: self.story_title,
            world_id: self.world_id,
            world_title: self.world_title,
            character_id: self.character_id,
            character_name: self.character_name,
            kind: self.kind,
            created_at: self.created_at,
            updated_at: self.updated_at,
            matched_fields,
            snippet,
            score: self.score,
        }
    }
}

pub(crate) fn metadata_from_payload(payload: &str) -> ChatMetadata {
    let data: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let world = data
        .get("session")
        .and_then(|session| session.get("world"))
        .unwrap_or(&Value::Null);
    let story_id = text_at(world, &["story_id"]);
    let story_title = text_at(world, &["story_title"]);
    let world_id = text_at(world, &["world_ref", "id"]);
    let world_title = first_nonempty([
        text_at(world, &["world_canon", "world_lore", "name"]),
        if story_id == "procedural" {
            story_title.clone()
        } else {
            String::new()
        },
        world_id.clone(),
    ]);
    let character_id = text_at(world, &["char_ref", "id"]);
    let character_name = text_at(world, &["player_character", "name"]);
    ChatMetadata {
        kind: if story_id == "procedural" {
            "world".to_string()
        } else {
            "chat".to_string()
        },
        story_id,
        story_title,
        world_id,
        world_title,
        character_id,
        character_name,
        visible_messages: visible_message_text(data.get("transcript")),
    }
}

fn text_at(value: &Value, path: &[&str]) -> String {
    let mut current = value;
    for key in path {
        current = match current.get(*key) {
            Some(value) => value,
            None => return String::new(),
        };
    }
    current.as_str().unwrap_or_default().trim().to_string()
}

fn first_nonempty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default()
}

/// Extract exactly the stable player-facing message surfaces. In particular,
/// do not index deltas, GM/NPC reasoning, claims, tool inputs/results, or debug
/// records: matching those would disclose text the current UI may hide.
fn visible_message_text(transcript: Option<&Value>) -> String {
    let rows = transcript
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut messages = Vec::new();
    for row in rows {
        let event = match row.get("event").and_then(Value::as_object) {
            Some(event) => event,
            None => continue,
        };
        let kind = event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = event.get("data").unwrap_or(&Value::Null);
        let message = match kind {
            "player" | "gm_narration" => data.as_str().unwrap_or_default().to_string(),
            "npc_speech" => visible_npc_speech(data),
            "scene_update" => visible_scene_update(data),
            "npc_whereabouts" => visible_whereabouts(data),
            _ => String::new(),
        };
        let message = collapse_whitespace(&message);
        if !message.is_empty() {
            messages.push(message);
        }
    }
    messages.join("\n")
}

fn visible_npc_speech(data: &Value) -> String {
    let response = text_at(data, &["response"]);
    if !response.is_empty() {
        return response;
    }
    [text_at(data, &["action"]), text_at(data, &["speech"])]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn visible_scene_update(data: &Value) -> String {
    first_nonempty([
        text_at(data, &["title"]),
        text_at(data, &["scene_id"]),
        text_at(data, &["name"]),
    ])
}

fn visible_whereabouts(data: &Value) -> String {
    let whereabouts = data.get("whereabouts").unwrap_or(&Value::Null);
    [
        text_at(data, &["name"]),
        text_at(whereabouts, &["status"]),
        first_nonempty([
            text_at(whereabouts, &["location_name"]),
            text_at(whereabouts, &["location_id"]),
        ]),
        text_at(whereabouts, &["details"]),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

fn normalize_for_search(text: &str) -> String {
    let normalized: String = text
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if ch == 'ё' { 'е' } else { ch })
        .collect();
    collapse_whitespace(&normalized)
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn search_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in normalize_for_search(text).chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn fts_expression(tokens: &[String], scope: ChatSearchScope) -> Option<String> {
    if tokens.is_empty() {
        return None;
    }
    let column = match scope {
        ChatSearchScope::All => None,
        ChatSearchScope::Title => Some("title"),
        ChatSearchScope::World => Some("world"),
        ChatSearchScope::Story => Some("story"),
        ChatSearchScope::Character => Some("character"),
        ChatSearchScope::Messages => Some("messages"),
    };
    Some(
        tokens
            .iter()
            .map(|token| {
                let token = token.replace('"', "\"\"");
                match column {
                    Some(column) => format!("{column}:\"{token}\"*"),
                    None => format!("\"{token}\"*"),
                }
            })
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

fn field_matches_any(text: &str, tokens: &[String]) -> bool {
    let words = search_tokens(text);
    tokens
        .iter()
        .any(|query| words.iter().any(|word| word.starts_with(query)))
}

fn message_snippet(messages: &str, tokens: &[String]) -> String {
    let line = messages
        .lines()
        .find(|line| field_matches_any(line, tokens))
        .unwrap_or_default();
    truncate_chars(&collapse_whitespace(line), 220)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_source_schema(con: &Connection) {
        con.execute_batch(
            "CREATE TABLE dialog_chats (
                 guest_id TEXT NOT NULL,
                 chat_id TEXT NOT NULL,
                 title TEXT NOT NULL,
                 preview TEXT NOT NULL,
                 turn_count INTEGER NOT NULL DEFAULT 0,
                 payload TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY (guest_id, chat_id)
             );
             CREATE TABLE guest_dialog_state (
                 guest_id TEXT PRIMARY KEY,
                 active_chat_id TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );",
        )
        .unwrap();
    }

    fn payload(visible: &str, hidden: &str) -> String {
        serde_json::to_string(&json!({
            "session": {"world": {
                "story_id": "story-1",
                "story_title": "Тайна Тёрнвейла",
                "world_ref": {"id": "world-1", "version": 2},
                "world_canon": {"world_lore": {"name": "Северный предел"}},
                "char_ref": {"id": "char-1", "version": 1},
                "player_character": {"name": "Ариан"}
            }},
            "transcript": [
                {"turn": 1, "event": {"kind": "player", "data": "Открываю дверь"}},
                {"turn": 1, "event": {"kind": "gm_narration", "data": visible}},
                {"turn": 1, "event": {"kind": "gm_thinking", "data": hidden}},
                {"turn": 1, "event": {"kind": "npc_thinking", "data": hidden}},
                {"turn": 1, "event": {"kind": "gm_tool_call", "data": {"arguments": hidden}}},
                {"turn": 1, "event": {"kind": "npc_speech", "data": {
                    "response": "Я всё видел", "hidden": hidden, "claims": [hidden]
                }}}
            ]
        }))
        .unwrap()
    }

    fn insert_source(con: &Connection, guest: &str, chat: &str, payload: &str) {
        con.execute(
            "INSERT INTO dialog_chats (
                 guest_id, chat_id, title, preview, turn_count, payload, created_at, updated_at
             ) VALUES (?1, ?2, 'Убийство в Тёрнвейле', 'Последняя сцена', 2, ?3,
                       '2026-07-14 10:00:00', '2026-07-15 10:00:00')",
            params![guest, chat, payload],
        )
        .unwrap();
        con.execute(
            "INSERT OR REPLACE INTO guest_dialog_state (
                 guest_id, active_chat_id, created_at, updated_at
             ) VALUES (?1, ?2, '2026-07-14 10:00:00', '2026-07-15 10:00:00')",
            params![guest, chat],
        )
        .unwrap();
    }

    #[test]
    fn metadata_and_visible_text_never_include_hidden_state() {
        let raw = payload("На стене алый символ", "СЕКРЕТНЫЙ_ПАРОЛЬ");
        let meta = metadata_from_payload(&raw);
        assert_eq!(meta.world_id, "world-1");
        assert_eq!(meta.world_title, "Северный предел");
        assert_eq!(meta.story_title, "Тайна Тёрнвейла");
        assert_eq!(meta.character_id, "char-1");
        assert_eq!(meta.character_name, "Ариан");
        assert!(meta.visible_messages.contains("алый символ"));
        assert!(meta.visible_messages.contains("Я всё видел"));
        assert!(!meta.visible_messages.contains("СЕКРЕТНЫЙ_ПАРОЛЬ"));
    }

    #[test]
    fn backfill_searches_cyrillic_prefix_and_folds_yo() {
        let con = Connection::open_in_memory().unwrap();
        create_source_schema(&con);
        insert_source(
            &con,
            "guest-a",
            "chat-a",
            &payload("Гаснет фонарь", "тайный план"),
        );
        initialize(&con).unwrap();

        let page = search(
            &con,
            "guest-a",
            &ChatSearchQuery {
                text: "терн".to_string(),
                scope: ChatSearchScope::All,
                ..ChatSearchQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].id, "chat-a");
        assert!(page.hits[0].matched_fields.contains(&"title".to_string()));

        let message_page = search(
            &con,
            "guest-a",
            &ChatSearchQuery {
                text: "фон".to_string(),
                scope: ChatSearchScope::Messages,
                ..ChatSearchQuery::default()
            },
        )
        .unwrap();
        assert_eq!(message_page.total, 1);
        assert!(message_page.hits[0].snippet.contains("Гаснет фонарь"));

        let hidden_page = search(
            &con,
            "guest-a",
            &ChatSearchQuery {
                text: "тайный".to_string(),
                ..ChatSearchQuery::default()
            },
        )
        .unwrap();
        assert_eq!(hidden_page.total, 0);
    }

    #[test]
    fn filters_are_guest_scoped_and_delete_removes_fts_row() {
        let con = Connection::open_in_memory().unwrap();
        create_source_schema(&con);
        let raw = payload("Общий видимый текст", "скрыто");
        insert_source(&con, "guest-a", "chat-a", &raw);
        insert_source(&con, "guest-b", "chat-b", &raw);
        initialize(&con).unwrap();

        let query = ChatSearchQuery {
            text: "общий".to_string(),
            world_id: Some("world-1".to_string()),
            character_id: Some("char-1".to_string()),
            has_messages: Some(true),
            ..ChatSearchQuery::default()
        };
        let page = search(&con, "guest-a", &query).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].id, "chat-a");
        assert!(page.hits[0].active);

        let tx = con.unchecked_transaction().unwrap();
        delete_document(&tx, "guest-a", "chat-a").unwrap();
        tx.execute(
            "DELETE FROM dialog_chats WHERE guest_id = 'guest-a' AND chat_id = 'chat-a'",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(search(&con, "guest-a", &query).unwrap().total, 0);
        assert_eq!(search(&con, "guest-b", &query).unwrap().total, 1);
    }
}
