//! Unified application search.
//!
//! Library packages stay authoritative in their existing filesystem stores.
//! Chat text is searched through the SQLite FTS projection owned by
//! `gml-persistence`.  Keeping the composition here avoids coupling the
//! package stores to the dialogs database and gives every UI surface one wire
//! contract.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use gml_persistence::{ChatSearchQuery, ChatSearchScope, ChatSearchSort};

use crate::{chat_scope_id, json_response, AppState};

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
const MAX_OFFSET: usize = 500;
const MAX_QUERY_CHARS: usize = 160;

#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub q: String,
    pub scope: Option<String>,
    pub types: Option<String>,
    pub fields: Option<String>,
    /// Singular alias used by the compact chat filter UI.
    pub field: Option<String>,
    pub world_id: Option<String>,
    pub story_id: Option<String>,
    pub character_id: Option<String>,
    pub kind: Option<String>,
    pub period: Option<String>,
    pub has_messages: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchItem {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: String,
    pub subtitle: String,
    pub snippet: String,
    pub matched_fields: Vec<String>,
    pub updated_at: String,
    pub turn_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// A unified, response-local score. It is useful only for ordering this
    /// response and is deliberately not a stable relevance metric.
    pub score: f64,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    ok: bool,
    query: String,
    items: Vec<SearchItem>,
    total: usize,
    has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestScope {
    All,
    Library,
    Chats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ItemType {
    World,
    Story,
    Character,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchField {
    All,
    Title,
    World,
    Story,
    Character,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchOrder {
    Relevance,
    Updated,
}

struct ParsedSearch {
    query: String,
    query_normalized: String,
    scope: RequestScope,
    types: HashSet<ItemType>,
    field: SearchField,
    world_id: Option<String>,
    story_id: Option<String>,
    character_id: Option<String>,
    kind: Option<String>,
    updated_after: Option<String>,
    has_messages: Option<bool>,
    order: SearchOrder,
    limit: usize,
    offset: usize,
}

impl ParsedSearch {
    fn parse(params: SearchParams) -> Result<Self, String> {
        let query = params.q.trim().to_string();
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(format!("q must be at most {MAX_QUERY_CHARS} characters"));
        }

        let scope = match params.scope.as_deref().unwrap_or("all").trim() {
            "" | "all" => RequestScope::All,
            "library" => RequestScope::Library,
            "chats" | "chat" => RequestScope::Chats,
            other => return Err(format!("unsupported search scope: {other}")),
        };

        let mut types = default_types(scope);
        if let Some(raw) = params
            .types
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            let requested = parse_types(raw)?;
            types.retain(|item_type| requested.contains(item_type));
        }

        let raw_field = params
            .fields
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or(params.field.as_deref())
            .unwrap_or("all");
        let field = parse_field(raw_field)?;

        let order = match params.sort.as_deref().unwrap_or("relevance").trim() {
            "" | "relevance" => SearchOrder::Relevance,
            "updated" => SearchOrder::Updated,
            other => return Err(format!("unsupported search sort: {other}")),
        };

        let has_messages = match params.has_messages.as_deref() {
            None | Some("") => None,
            Some("true" | "1" | "yes") => Some(true),
            Some("false" | "0" | "no") => Some(false),
            Some(other) => return Err(format!("invalid has_messages value: {other}")),
        };

        let updated_after = parse_period(params.period.as_deref())?;
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = params.offset.unwrap_or(0).min(MAX_OFFSET);

        Ok(ParsedSearch {
            query_normalized: normalize_search_text(&query),
            query,
            scope,
            types,
            field,
            world_id: clean_filter(params.world_id),
            story_id: clean_filter(params.story_id),
            character_id: clean_filter(params.character_id),
            kind: clean_filter(params.kind),
            updated_after,
            has_messages,
            order,
            limit,
            offset,
        })
    }

    fn includes(&self, item_type: ItemType) -> bool {
        self.types.contains(&item_type)
    }

    fn chat_query(&self, limit: usize, offset: usize) -> ChatSearchQuery {
        ChatSearchQuery {
            text: self.query.clone(),
            scope: match self.field {
                SearchField::All => ChatSearchScope::All,
                SearchField::Title => ChatSearchScope::Title,
                SearchField::World => ChatSearchScope::World,
                SearchField::Story => ChatSearchScope::Story,
                SearchField::Character => ChatSearchScope::Character,
                SearchField::Messages => ChatSearchScope::Messages,
            },
            world_id: self.world_id.clone(),
            story_id: self.story_id.clone(),
            character_id: self.character_id.clone(),
            kind: self.kind.clone(),
            updated_after: self.updated_after.clone(),
            has_messages: self.has_messages,
            sort: match self.order {
                SearchOrder::Relevance => ChatSearchSort::Relevance,
                SearchOrder::Updated => ChatSearchSort::Updated,
            },
            limit,
            offset,
        }
    }
}

pub async fn get_search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Response {
    let request = match ParsedSearch::parse(params) {
        Ok(request) => request,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"ok": false, "error": error}),
            )
        }
    };

    let worker_state = state.clone();
    let result = tokio::task::spawn_blocking(move || execute_search(&worker_state, &request)).await;
    match result {
        Ok(Ok(response)) => json_response(
            StatusCode::OK,
            &serde_json::to_value(response).unwrap_or_else(|_| {
                json!({
                    "ok": false,
                    "error": "failed to serialize search response",
                })
            }),
        ),
        Ok(Err(error)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": error}),
        ),
        Err(error) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {error}")}),
        ),
    }
}

fn execute_search(state: &AppState, request: &ParsedSearch) -> Result<SearchResponse, String> {
    let only_chats = request.includes(ItemType::Chat)
        && !request.includes(ItemType::World)
        && !request.includes(ItemType::Story)
        && !request.includes(ItemType::Character);

    if only_chats {
        let page = state
            .store
            .search_chats(
                &chat_scope_id(),
                &request.chat_query(request.limit, request.offset),
            )
            .map_err(|error| error.to_string())?;
        return Ok(SearchResponse {
            ok: true,
            query: request.query.clone(),
            items: page
                .hits
                .into_iter()
                .map(|hit| chat_item(hit, &request.query_normalized))
                .collect(),
            total: page.total,
            has_more: page.has_more,
        });
    }

    let mut items = library_items(state, request)?;
    let library_total = items.len();
    let mut chat_total = 0usize;

    if request.includes(ItemType::Chat) {
        // Mixed global results must be sorted after composition. Fetch only the
        // leading chat window needed for the requested unified page, in bounded
        // chunks; the ordinary chats-only path above retains true DB paging.
        let target = (request.offset + request.limit).min(MAX_OFFSET + MAX_LIMIT);
        let mut chat_offset = 0usize;
        while chat_offset < target {
            let chunk = (target - chat_offset).min(MAX_LIMIT);
            let page = state
                .store
                .search_chats(&chat_scope_id(), &request.chat_query(chunk, chat_offset))
                .map_err(|error| error.to_string())?;
            chat_total = page.total;
            let received = page.hits.len();
            items.extend(
                page.hits
                    .into_iter()
                    .map(|hit| chat_item(hit, &request.query_normalized)),
            );
            chat_offset += received;
            if received == 0 || !page.has_more {
                break;
            }
        }
    }

    sort_items(&mut items, request.order);
    let total = library_total.saturating_add(chat_total);
    let page = items
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
        .collect::<Vec<_>>();
    let has_more = request.offset.saturating_add(page.len()) < total;

    Ok(SearchResponse {
        ok: true,
        query: request.query.clone(),
        items: page,
        total,
        has_more,
    })
}

fn library_items(state: &AppState, request: &ParsedSearch) -> Result<Vec<SearchItem>, String> {
    if request.scope == RequestScope::Chats
        || matches!(request.field, SearchField::Messages)
        || (!request.includes(ItemType::World)
            && !request.includes(ItemType::Story)
            && !request.includes(ItemType::Character))
    {
        return Ok(Vec::new());
    }

    let worlds = state
        .world_store
        .list_worlds()
        .map_err(|error| error.to_string())?;
    let stories = {
        let store = state.story_store.lock().expect("story store lock poisoned");
        store.list_stories()
    };
    let characters = {
        let store = state
            .character_store
            .lock()
            .expect("character store lock poisoned");
        store.list_characters()
    };

    let world_titles = worlds
        .iter()
        .filter_map(|world| Some((value_string(world, "id")?, value_string(world, "title")?)))
        .collect::<HashMap<_, _>>();
    let story_titles = stories
        .iter()
        .filter_map(|story| {
            Some((
                story.get("id")?.as_str()?.to_string(),
                story.get("title")?.as_str()?.to_string(),
            ))
        })
        .collect::<HashMap<_, _>>();

    let mut items = Vec::new();
    if request.includes(ItemType::World) {
        for world in &worlds {
            if let Some(item) = world_item(world, request) {
                items.push(item);
            }
        }
    }
    if request.includes(ItemType::Story) {
        for story in &stories {
            if let Some(item) = story_item(story, &world_titles, request) {
                items.push(item);
            }
        }
    }
    if request.includes(ItemType::Character) {
        for character in &characters {
            if let Some(item) = character_item(character, &world_titles, &story_titles, request) {
                items.push(item);
            }
        }
    }
    Ok(items)
}

fn world_item(world: &Value, request: &ParsedSearch) -> Option<SearchItem> {
    let id = value_string(world, "id")?;
    let title = value_string(world, "title").unwrap_or_else(|| "Новый мир".to_string());
    let subtitle = value_string(world, "preview").unwrap_or_default();
    if !filter_matches(request.world_id.as_deref(), Some(&id))
        || request.story_id.is_some()
        || request.character_id.is_some()
    {
        return None;
    }

    let mut world_texts = Vec::new();
    for key in [
        "preview",
        "genre",
        "tone",
        "world_size",
        "population",
        "public_premise",
    ] {
        push_value_strings(world.get(key), &mut world_texts);
    }
    if let Some(lore) = world.get("world_lore") {
        for key in ["name", "public_premise", "regions"] {
            push_value_strings(lore.get(key), &mut world_texts);
        }
    }

    let fields = match request.field {
        SearchField::All => vec![("title", vec![title.clone()]), ("world", world_texts)],
        SearchField::Title => vec![("title", vec![title.clone()])],
        SearchField::World => vec![("world", {
            let mut values = vec![title.clone()];
            values.extend(world_texts);
            values
        })],
        _ => return None,
    };
    let matched = match_fields(&request.query_normalized, &fields)?;

    Some(SearchItem {
        id: id.clone(),
        item_type: "world".to_string(),
        title: title.clone(),
        subtitle: subtitle.clone(),
        snippet: matched.snippet.unwrap_or_else(|| subtitle.clone()),
        matched_fields: matched.fields,
        updated_at: value_string(world, "updated_at").unwrap_or_default(),
        turn_count: 0,
        world_id: Some(id),
        world_title: Some(title),
        story_id: None,
        story_title: None,
        character_id: None,
        character_title: None,
        chat_kind: None,
        active: None,
        score: matched.score,
    })
}

fn story_item(
    story: &serde_json::Map<String, Value>,
    world_titles: &HashMap<String, String>,
    request: &ParsedSearch,
) -> Option<SearchItem> {
    let id = story.get("id")?.as_str()?.to_string();
    let title = story
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Новая история")
        .to_string();
    let description = story
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let brief = story
        .get("story_brief")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let world_id = nested_id(story.get("world_ref"));
    let world_title = world_id
        .as_ref()
        .and_then(|id| world_titles.get(id))
        .cloned();

    if !filter_matches(request.world_id.as_deref(), world_id.as_deref())
        || !filter_matches(request.story_id.as_deref(), Some(&id))
        || request.character_id.is_some()
    {
        return None;
    }

    let story_texts = vec![description.clone(), brief.clone()];
    let world_texts = [world_id.clone(), world_title.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let fields = match request.field {
        SearchField::All => vec![
            ("title", vec![title.clone()]),
            ("story", story_texts),
            ("world", world_texts),
        ],
        SearchField::Title => vec![("title", vec![title.clone()])],
        SearchField::Story => vec![("story", {
            let mut values = vec![title.clone()];
            values.extend(story_texts);
            values
        })],
        SearchField::World => vec![("world", world_texts)],
        _ => return None,
    };
    let matched = match_fields(&request.query_normalized, &fields)?;
    let subtitle = if description.trim().is_empty() {
        brief.clone()
    } else {
        description.clone()
    };

    Some(SearchItem {
        id: id.clone(),
        item_type: "story".to_string(),
        title: title.clone(),
        subtitle: subtitle.clone(),
        snippet: matched.snippet.unwrap_or_else(|| subtitle.clone()),
        matched_fields: matched.fields,
        updated_at: String::new(),
        turn_count: 0,
        world_id,
        world_title,
        story_id: Some(id),
        story_title: Some(title),
        character_id: None,
        character_title: None,
        chat_kind: story
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string),
        active: None,
        score: matched.score,
    })
}

fn character_item(
    character: &Value,
    world_titles: &HashMap<String, String>,
    story_titles: &HashMap<String, String>,
    request: &ParsedSearch,
) -> Option<SearchItem> {
    let id = value_string(character, "id")?;
    let title = value_string(character, "title").unwrap_or_else(|| "Персонаж".to_string());
    let preview = value_string(character, "preview").unwrap_or_default();
    let world_id = nested_id(character.get("world_ref"));
    let story_id = nested_id(character.get("story_ref"));
    let world_title = world_id
        .as_ref()
        .and_then(|id| world_titles.get(id))
        .cloned();
    let story_title = story_id
        .as_ref()
        .and_then(|id| story_titles.get(id))
        .cloned();

    if !filter_matches(request.world_id.as_deref(), world_id.as_deref())
        || !filter_matches(request.story_id.as_deref(), story_id.as_deref())
        || !filter_matches(request.character_id.as_deref(), Some(&id))
    {
        return None;
    }

    let mut character_texts = vec![preview.clone()];
    if let Some(pc) = character
        .get("payload")
        .and_then(|payload| payload.get("player_character"))
    {
        // Player-visible sheet fields only. In particular, `gm_notes` and the
        // rest of the opaque package payload never enter global snippets.
        for key in [
            "name",
            "class_role",
            "background",
            "age",
            "physical_type",
            "distinctive_features",
            "current_appearance",
            "life_status",
            "life_status_note",
            "condition",
            "personality",
            "values",
            "languages",
            "inventory",
            "equipment",
            "features",
            "spells",
        ] {
            push_value_strings(pc.get(key), &mut character_texts);
        }
    }
    let world_texts = [world_id.clone(), world_title.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let story_texts = [story_id.clone(), story_title.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let fields = match request.field {
        SearchField::All => vec![
            ("title", vec![title.clone()]),
            ("character", character_texts),
            ("world", world_texts),
            ("story", story_texts),
        ],
        SearchField::Title => vec![("title", vec![title.clone()])],
        SearchField::Character => vec![("character", {
            let mut values = vec![title.clone()];
            values.extend(character_texts);
            values
        })],
        SearchField::World => vec![("world", world_texts)],
        SearchField::Story => vec![("story", story_texts)],
        _ => return None,
    };
    let matched = match_fields(&request.query_normalized, &fields)?;

    Some(SearchItem {
        id: id.clone(),
        item_type: "character".to_string(),
        title: title.clone(),
        subtitle: preview.clone(),
        snippet: matched.snippet.unwrap_or_else(|| preview.clone()),
        matched_fields: matched.fields,
        updated_at: value_string(character, "updated_at").unwrap_or_default(),
        turn_count: 0,
        world_id,
        world_title,
        story_id,
        story_title,
        character_id: Some(id),
        character_title: Some(title),
        chat_kind: None,
        active: None,
        score: matched.score,
    })
}

fn chat_item(hit: gml_persistence::ChatSearchHit, query_normalized: &str) -> SearchItem {
    let title_normalized = normalize_search_text(&hit.title);
    let title_score = if !query_normalized.is_empty() && title_normalized == query_normalized {
        1_000.0
    } else if !query_normalized.is_empty() && title_normalized.starts_with(query_normalized) {
        800.0
    } else if hit.matched_fields.iter().any(|field| field == "title") {
        600.0
    } else if hit
        .matched_fields
        .iter()
        .any(|field| matches!(field.as_str(), "world" | "story" | "character"))
    {
        500.0
    } else if hit.matched_fields.iter().any(|field| field == "messages") {
        300.0
    } else {
        100.0
    };
    let tie_break = if hit.score.is_finite() {
        hit.score.clamp(0.0, 99.0) / 100.0
    } else {
        0.0
    };
    let context = [
        non_empty(&hit.story_title),
        non_empty(&hit.world_title),
        non_empty(&hit.character_name),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let subtitle = if !hit.preview.trim().is_empty() {
        hit.preview.clone()
    } else {
        context
    };

    SearchItem {
        id: hit.id,
        item_type: "chat".to_string(),
        title: hit.title,
        subtitle,
        snippet: hit.snippet,
        matched_fields: hit.matched_fields,
        updated_at: hit.updated_at,
        turn_count: hit.turn_count,
        world_id: non_empty(&hit.world_id),
        world_title: non_empty(&hit.world_title),
        story_id: non_empty(&hit.story_id),
        story_title: non_empty(&hit.story_title),
        character_id: non_empty(&hit.character_id),
        character_title: non_empty(&hit.character_name),
        chat_kind: non_empty(&hit.kind),
        active: Some(hit.active),
        score: title_score + tie_break,
    }
}

struct FieldMatch {
    fields: Vec<String>,
    snippet: Option<String>,
    score: f64,
}

fn match_fields(query: &str, fields: &[(&str, Vec<String>)]) -> Option<FieldMatch> {
    if query.is_empty() {
        return Some(FieldMatch {
            fields: Vec::new(),
            snippet: None,
            score: 0.0,
        });
    }
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let mut matched = Vec::new();
    let mut snippet = None;
    let mut score: f64 = 0.0;
    for (field, values) in fields {
        let normalized_values = values
            .iter()
            .map(|value| normalize_search_text(value))
            .collect::<Vec<_>>();
        let corpus = normalized_values.join(" ");
        if !tokens.iter().all(|token| corpus.contains(token)) {
            continue;
        }
        let value = values
            .iter()
            .zip(&normalized_values)
            .find(|(_, value)| tokens.iter().any(|token| value.contains(token)))
            .map(|(value, _)| value)
            .or_else(|| values.first())?;
        matched.push((*field).to_string());
        if snippet.is_none() && *field != "title" {
            snippet = Some(truncate_text(value, 220));
        }
        let normalized = normalize_search_text(value);
        let field_score: f64 = if *field == "title" {
            if normalized == query {
                1_000.0
            } else if normalized.starts_with(query) {
                800.0
            } else {
                600.0
            }
        } else if matches!(*field, "world" | "story" | "character") {
            500.0
        } else {
            300.0
        };
        score = score.max(field_score);
    }
    if matched.is_empty() {
        None
    } else {
        Some(FieldMatch {
            fields: matched,
            snippet,
            score,
        })
    }
}

fn sort_items(items: &mut [SearchItem], order: SearchOrder) {
    items.sort_by(|left, right| match order {
        SearchOrder::Relevance => right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.item_type.cmp(&right.item_type)),
        SearchOrder::Updated => right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| left.title.cmp(&right.title)),
    });
}

fn default_types(scope: RequestScope) -> HashSet<ItemType> {
    match scope {
        RequestScope::All => [
            ItemType::World,
            ItemType::Story,
            ItemType::Character,
            ItemType::Chat,
        ]
        .into_iter()
        .collect(),
        RequestScope::Library => [ItemType::World, ItemType::Story, ItemType::Character]
            .into_iter()
            .collect(),
        RequestScope::Chats => [ItemType::Chat].into_iter().collect(),
    }
}

fn parse_types(raw: &str) -> Result<HashSet<ItemType>, String> {
    let mut types = HashSet::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let item_type = match value {
            "world" | "worlds" => ItemType::World,
            "story" | "stories" => ItemType::Story,
            "character" | "characters" => ItemType::Character,
            "chat" | "chats" => ItemType::Chat,
            other => return Err(format!("unsupported search type: {other}")),
        };
        types.insert(item_type);
    }
    Ok(types)
}

fn parse_field(raw: &str) -> Result<SearchField, String> {
    let values = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.len() > 1 {
        // The persistence index intentionally exposes one weighted scope at a
        // time. Multiple chips mean the ordinary all-fields union.
        for value in &values {
            validate_field(value)?;
        }
        return Ok(SearchField::All);
    }
    let value = values.first().copied().unwrap_or("all");
    validate_field(value)
}

fn validate_field(value: &str) -> Result<SearchField, String> {
    match value {
        "" | "all" => Ok(SearchField::All),
        "title" => Ok(SearchField::Title),
        "world" => Ok(SearchField::World),
        "story" => Ok(SearchField::Story),
        "character" => Ok(SearchField::Character),
        "messages" | "message" => Ok(SearchField::Messages),
        other => Err(format!("unsupported search field: {other}")),
    }
}

fn parse_period(period: Option<&str>) -> Result<Option<String>, String> {
    match period.unwrap_or("all").trim() {
        "" | "all" | "any" => Ok(None),
        "today" | "day" => Ok(Some(utc_timestamp_days_ago(0, true))),
        "week" | "7d" => Ok(Some(utc_timestamp_days_ago(7, false))),
        "month" | "30d" => Ok(Some(utc_timestamp_days_ago(30, false))),
        "quarter" | "90d" => Ok(Some(utc_timestamp_days_ago(90, false))),
        other => Err(format!("unsupported search period: {other}")),
    }
}

fn clean_filter(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn filter_matches(filter: Option<&str>, actual: Option<&str>) -> bool {
    filter.is_none() || filter == actual
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn nested_id(value: Option<&Value>) -> Option<String> {
    value?
        .get("id")?
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn push_value_strings(value: Option<&Value>, out: &mut Vec<String>) {
    match value {
        Some(Value::String(value)) if !value.trim().is_empty() => out.push(value.clone()),
        Some(Value::Array(values)) => {
            for value in values {
                push_value_strings(Some(value), out);
            }
        }
        Some(Value::Object(values)) => {
            for value in values.values() {
                push_value_strings(Some(value), out);
            }
        }
        _ => {}
    }
}

fn normalize_search_text(value: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        let character = if character == 'ё' { 'е' } else { character };
        if character.is_alphanumeric() {
            if separator && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    normalized
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= max_chars {
        value
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn utc_timestamp_days_ago(days: u64, start_of_day: bool) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut seconds = now.saturating_sub(days.saturating_mul(86_400));
    if start_of_day {
        seconds -= seconds % 86_400;
    }
    let day = (seconds / 86_400) as i64;
    let time = (seconds % 86_400) as i64;
    let (year, month, date) = civil_date_from_days(day);
    format!(
        "{year:04}-{month:02}-{date:02} {:02}:{:02}:{:02}",
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

fn civil_date_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_case_and_yo_insensitive() {
        assert_eq!(normalize_search_text(" ЁЖИК, Мир! "), "ежик мир");
    }

    #[test]
    fn civil_date_epoch_is_correct() {
        assert_eq!(civil_date_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn query_limit_is_clamped() {
        let parsed = ParsedSearch::parse(SearchParams {
            q: "мир".to_string(),
            limit: Some(500),
            ..SearchParams::default()
        })
        .unwrap();
        assert_eq!(parsed.limit, MAX_LIMIT);
    }

    #[test]
    fn chat_filter_periods_match_the_ui() {
        for period in ["7d", "30d", "90d"] {
            let parsed = ParsedSearch::parse(SearchParams {
                period: Some(period.to_string()),
                ..SearchParams::default()
            })
            .unwrap();
            assert!(parsed.updated_after.is_some(), "period {period}");
        }
    }

    #[test]
    fn character_search_includes_current_appearance() {
        let request = ParsedSearch::parse(SearchParams {
            q: "промокший дорожный плащ".to_string(),
            types: Some("character".to_string()),
            field: Some("character".to_string()),
            ..SearchParams::default()
        })
        .unwrap();
        let character = json!({
            "id": "darra",
            "title": "Дарра",
            "payload": {
                "player_character": {
                    "current_appearance": "промокший дорожный плащ"
                }
            }
        });

        let item = character_item(&character, &HashMap::new(), &HashMap::new(), &request)
            .expect("current appearance must be searchable");
        assert!(item.matched_fields.iter().any(|field| field == "character"));
    }
}
