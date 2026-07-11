//! Generic architect agent loop (`docs/CHARACTERS_AND_STORY_TZ.md` §С1.2).
//!
//! This is the shared engine behind the WORLD architect ([`crate::world_architect`])
//! and the STORY architect ([`crate::story_architect`]): the two are thin CONFIGS
//! over this one loop, not forks. Everything here is domain-agnostic — the hop
//! sink, call normalization, tool-call plumbing, stat accumulation, history
//! filtering, and the think → tool → reply agent loop that mirrors the GM turn's
//! `max_tool_hops`.
//!
//! What VARIES between the two architects is captured by [`ArchitectConfig`]:
//! - the system prompt,
//! - the tool schemas,
//! - how the incoming draft is normalized to the loop's working shape,
//! - how the user message is built,
//! - how each tool call folds into the working draft (`apply_tool`),
//! - how the finished draft is finalized,
//! - the per-tool result text fed back to the model.
//!
//! REGRESSION GATE: the world path MUST stay byte-identical — the extraction is
//! behavior-preserving. The world config produces the same messages, tools, tool
//! results and draft-folding as before; the loop below is a verbatim lift of the
//! former `world_architect_turn_with_options` body.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError, DeltaSink};
use gml_types::Role;

/// Safety cap on the architect agent loop (think → draft → reply, possibly
/// refined across several tool calls). Mirrors the GM turn's `max_tool_hops`: a
/// real model normally ends in 2-3 hops, but a degenerate model that keeps
/// calling the tool must still terminate.
pub const MAX_ARCHITECT_HOPS: usize = 6;

/// Streaming sink for the architect agent loop. The server implements this to
/// forward segments to the SSE client; tests can use a no-op. Each segment is
/// tagged with a `sid` (one per agent hop) so the UI groups deltas into separate
/// reasoning spoilers and reply bubbles, exactly like the main GM turn.
pub trait ArchitectStream {
    /// A content/thinking delta for the current hop. `channel` is
    /// [`gml_llm::channel::THINKING`] or [`gml_llm::channel::CONTENT`].
    fn delta(&mut self, channel: &str, text: &str, sid: &str);
    /// A tool call the model just made, surfaced inline as it happens.
    fn tool(&mut self, call: &Value, sid: &str);
}

/// A no-op [`ArchitectStream`] for callers that don't need streaming.
pub struct NullArchitectStream;
impl ArchitectStream for NullArchitectStream {
    fn delta(&mut self, _channel: &str, _text: &str, _sid: &str) {}
    fn tool(&mut self, _call: &Value, _sid: &str) {}
}

/// Per-hop adapter so `chat_stream` (which wants a [`DeltaSink`]) forwards its
/// deltas to the architect stream tagged with this hop's `sid`.
struct HopSink<'a> {
    sid: String,
    inner: &'a mut (dyn ArchitectStream + Send),
}

impl DeltaSink for HopSink<'_> {
    fn emit(&mut self, ch: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.inner.delta(ch, text, &self.sid);
    }
}

/// The output of one architect turn — domain-agnostic. `draft` is the finalized
/// working draft (world bible OR story plot) when it changed this turn, else
/// `None`.
pub struct ArchitectOutput {
    /// The model's final chat reply (the last text segment of the turn).
    pub reply: String,
    pub draft: Option<Value>,
    pub user_msg: Value,
    pub assistant_history_msg: Value,
    pub assistant_msg: Value,
    pub calls: Vec<Value>,
    /// Ordered visible segments produced THIS turn — `think`, `assistant` and
    /// `tool` entries in production order — to append to the persisted chat and
    /// restore the interleaved view on reload.
    pub visible_segments: Vec<Value>,
    /// All reasoning text from the turn, joined (for the debug view).
    pub thinking: String,
    /// Summed `_meta` stats across every hop (cached_tokens, prompt_eval_count,
    /// eval_count) — drives the architect token-usage readout.
    pub stats: Map<String, Value>,
    /// The first-hop request messages array sent to the model (for the debug view).
    pub request_messages: Value,
}

/// The domain hook set that turns the generic loop into a concrete architect.
/// The two implementors ([`crate::world_architect`] and
/// [`crate::story_architect`]) supply their prompt, tools, and draft-folding.
///
/// `Send + Sync`: [`architect_turn`] holds `&dyn ArchitectConfig` across `.await`
/// points, and the server drives it inside a `tokio::spawn` (which requires the
/// future to be `Send`). Both concrete configs are trivially `Send + Sync`.
pub trait ArchitectConfig: Send + Sync {
    /// The static system prompt (canon-authoring rules for this domain).
    fn system_prompt(&self) -> &str;

    /// Extra STABLE system blocks placed right after the system prompt, before
    /// history — part of the cacheable prefix. The world architect returns none;
    /// the story architect returns its read-only bound-world lore block (`§С1.2`)
    /// so the cache prefix holds across turns. Default: none.
    fn extra_system_blocks(&self) -> Vec<String> {
        Vec::new()
    }

    /// The tool schemas the model may call this turn.
    fn tools(&self) -> Vec<Value>;

    /// Normalize the incoming draft (frontend shape) into the canonical shape the
    /// loop mutates and shows the model. This is the base edits apply onto.
    fn normalize_draft(&self, draft: &Value) -> Value;

    /// Build the user message shown to the model. CACHE INVARIANT: this must be
    /// byte-identical to the history entry the server stores for this turn
    /// (the raw user text) — the request prefix across turns stays stable only
    /// when sent == stored. State never rides here; the model reads it through
    /// the domain's read tool.
    fn user_message(&self, user_text: &str) -> Value {
        json!({"role": "user", "content": user_text.trim()})
    }

    /// Apply ONE tool call to the working draft and produce everything the loop
    /// needs from it: the args the event/card should show, whether the draft
    /// actually changed, and the MODEL-FACING result text. The result must state
    /// FACTS about what happened (what was set/added/removed, what missed) —
    /// never a blind "ok" — so the model can self-correct a missed edit; READ
    /// tools return the requested sections of the (post-call) working draft.
    fn apply_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
        working_draft: &mut Value,
    ) -> ToolApplication;

    /// Finalize the working draft at the end of the turn (mirror summary fields,
    /// etc.). Called only when the draft changed.
    fn finalize_draft(&self, draft: Value) -> Value;
}

/// One applied tool call: what to show, whether the draft changed, and the
/// model-facing result text.
pub struct ToolApplication {
    /// The args surfaced in the tool card/event (the world build shows the
    /// NESTED draft args; edits show the raw patch).
    pub args: Value,
    pub changed: bool,
    /// Plain-text result fed back to the model (facts, not JSON).
    pub result: String,
}

/// Assemble the model request messages: system + filtered history + the user
/// message. Shared by both architects' public `*_messages` helpers.
pub fn architect_messages_with_user(
    system_prompt: &str,
    history: &[Value],
    user_msg: Value,
) -> Vec<Value> {
    architect_messages_with_system_blocks(system_prompt, &[], history, user_msg)
}

/// Like [`architect_messages_with_user`] but with extra STABLE system blocks
/// spliced right after the system prompt (the story architect's read-only world
/// lore block). Those blocks stay part of the cacheable prefix.
pub fn architect_messages_with_system_blocks(
    system_prompt: &str,
    extra_system_blocks: &[String],
    history: &[Value],
    user_msg: Value,
) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": system_prompt})];
    for block in extra_system_blocks {
        if !block.trim().is_empty() {
            messages.push(json!({"role": "system", "content": block}));
        }
    }
    messages.extend(history.iter().filter_map(history_message));
    messages.push(user_msg);
    messages
}

/// Run one architect turn against `client` using the domain `config`. This body
/// is the behavior-preserving lift of the former `world_architect_turn_with_options`.
pub async fn architect_turn(
    config: &dyn ArchitectConfig,
    client: &dyn Backend,
    history: &[Value],
    draft: &Value,
    user_text: &str,
    stream: &mut (dyn ArchitectStream + Send),
) -> Result<ArchitectOutput, BackendError> {
    let user_msg = config.user_message(user_text);
    // The running model conversation: system (+ extra stable blocks) + history +
    // user, then assistant turns and tool results appended as the loop drives the
    // agent.
    let mut messages = architect_messages_with_system_blocks(
        config.system_prompt(),
        &config.extra_system_blocks(),
        history,
        user_msg.clone(),
    );
    let request_messages = Value::Array(messages.clone());
    let tools = Value::Array(config.tools());

    let mut visible_segments: Vec<Value> = Vec::new();
    let mut all_calls: Vec<Value> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    // The full draft state the agent mutates this turn — seeded from the current
    // draft so edit-tool patches apply to the real draft, not a blank one.
    let mut working_draft = config.normalize_draft(draft);
    let mut draft_changed = false;
    let mut reply = String::new();
    let mut stats = Map::new();

    let mut hop = 0usize;
    loop {
        hop += 1;
        let sid = format!("arch-{hop}");

        // Stream this hop. Thinking/content deltas are tagged with `sid` so the
        // UI renders a separate reasoning spoiler and reply bubble per hop.
        let output = {
            let mut seg = HopSink {
                sid: sid.clone(),
                inner: &mut *stream,
            };
            client
                .chat_stream(
                    &Value::Array(messages.clone()),
                    Some(&tools),
                    Some(true),
                    Role::Gm.as_str(),
                    &mut seg,
                )
                .await?
        };

        accumulate_stats(&mut stats, &output.stats);

        let thinking = output.thinking.trim().to_string();
        if !thinking.is_empty() {
            visible_segments.push(json!({"role": "think", "content": thinking, "sid": sid}));
            thinking_parts.push(thinking);
        }

        let content = output.content.trim().to_string();

        // No tool calls → this hop's text is the final reply; end the turn.
        if output.calls.is_empty() {
            if !content.is_empty() {
                visible_segments
                    .push(json!({"role": "assistant", "content": content.clone(), "sid": sid}));
                reply = content;
            }
            messages.push(output.assistant_msg);
            break;
        }

        // Intermediate text alongside a tool call (a "response between tools").
        if !content.is_empty() {
            visible_segments
                .push(json!({"role": "assistant", "content": content.clone(), "sid": sid}));
        }

        let normalized = normalize_architect_calls(&output.calls, &sid);
        messages.push(assistant_with_tool_calls(output.assistant_msg, &normalized));

        for (name, args, id) in &normalized {
            // The domain config folds the call into the working draft and returns
            // the args to show in the card/event plus the model-facing result.
            let applied = config.apply_tool(name, args, &mut working_draft);
            if applied.changed {
                draft_changed = true;
            }
            let call_json = json!({"name": name, "arguments": applied.args, "id": id});
            all_calls.push(call_json.clone());
            visible_segments
                .push(json!({"role": "tool", "name": name, "args": applied.args, "sid": sid}));
            stream.tool(&call_json, &sid);
            // Feed the result back so the model can keep refining or finish with a
            // chat reply (this is what makes it an agent loop, not a one-shot).
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": applied.result,
            }));
        }

        if hop >= MAX_ARCHITECT_HOPS {
            break;
        }
    }

    let assistant_history_msg = json!({"role": "assistant", "content": reply});
    Ok(ArchitectOutput {
        reply,
        // The full draft after this turn's build/edits (None if nothing changed).
        draft: if draft_changed {
            Some(config.finalize_draft(working_draft))
        } else {
            None
        },
        user_msg,
        assistant_history_msg: assistant_history_msg.clone(),
        assistant_msg: assistant_history_msg,
        calls: all_calls,
        visible_segments,
        thinking: thinking_parts.join("\n\n"),
        stats,
        request_messages,
    })
}

/// Assign a stable id to each call and return `(name, args, id)` tuples.
fn normalize_architect_calls(
    calls: &[gml_types::ParsedCall],
    sid: &str,
) -> Vec<(String, Map<String, Value>, String)> {
    calls
        .iter()
        .enumerate()
        .map(|(idx, call)| {
            let id = if call.id.trim().is_empty() {
                format!("{sid}_{}", idx + 1)
            } else {
                call.id.clone()
            };
            (call.name.clone(), call.arguments.clone(), id)
        })
        .collect()
}

/// Attach `tool_calls` to the assistant message so the next request is a valid
/// tool-call/tool-result pair (mirrors the orchestrator helper).
fn assistant_with_tool_calls(
    assistant_msg: Value,
    calls: &[(String, Map<String, Value>, String)],
) -> Value {
    let mut msg = match assistant_msg {
        Value::Object(m) => m,
        other => return other,
    };
    let raw_calls: Vec<Value> = calls
        .iter()
        .filter(|(name, _, _)| !name.trim().is_empty())
        .map(|(name, args, id)| {
            json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(&Value::Object(args.clone()))
                        .unwrap_or_else(|_| "{}".to_string()),
                },
            })
        })
        .collect();
    if !raw_calls.is_empty() {
        msg.insert("tool_calls".to_string(), Value::Array(raw_calls));
    }
    Value::Object(msg)
}

/// Sum integer `_meta` counters across hops (token counts add up like the main
/// chat's per-turn total); non-integer/last-wins for everything else.
fn accumulate_stats(acc: &mut Map<String, Value>, next: &Map<String, Value>) {
    for (key, value) in next {
        let summed = match (acc.get(key), value) {
            (Some(a), b) if a.is_i64() && b.is_i64() => Some(Value::from(
                a.as_i64().unwrap_or(0) + b.as_i64().unwrap_or(0),
            )),
            _ => None,
        };
        acc.insert(key.clone(), summed.unwrap_or_else(|| value.clone()));
    }
}

fn history_message(message: &Value) -> Option<Value> {
    let object = message.as_object()?;
    let role = object.get("role").and_then(Value::as_str)?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let content = object.get("content").and_then(Value::as_str)?.trim();
    if content.is_empty() {
        return None;
    }
    Some(json!({"role": role, "content": content}))
}
