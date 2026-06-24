//! Shared model-facing tool capability guidance.
//!
//! Faithful port of the stable tool-catalog loader guidance from
//! `gm-lab/tool_guidance.py`. The remaining
//! `tool_guidance` text already lives baked into the captured `GM_SYSTEM`
//! prompt (see `gml-prompts`), so it is not re-derived here.
//!
//! These are `pub(crate)` module constants — byte-for-byte copies of the
//! Python module-level strings, so the assembled tool JSON matches the golden
//! `gm_tools.json` fixture exactly.

pub(crate) const TOOL_SEARCH_DESCRIPTION: &str =
    "Search the compact GM tool catalog without loading full schemas. Use this when a \
needed scene, movement, NPC profile, whereabouts, memory, dice, or canon capability \
is not visible or you need to discover the exact tool name. Query with keywords or \
exact selection such as select:tool_name or select:tool_a,tool_b. The result is \
short catalog metadata only: name, title, description, keywords, aliases, \
capabilities, score, loaded status, and a hint to call load_tool_schema. It never \
returns full JSON schemas and does not load matching tools.";

pub(crate) const LOAD_TOOL_SCHEMA_DESCRIPTION: &str =
    "Load one exact GM tool schema returned by tool_search. Pass the exact canonical \
tool name, not keywords, aliases, or comma lists. The result confirms the full OpenAI \
function schema {type:\"function\", function:{name, description, parameters, strict?}} \
inside the tool result so the top-level tool list stays cache-stable. For non-visible \
tools, call invoke_loaded_tool next with the same name and schema-matching arguments. \
If the tool is already visible, the same full schema is returned as confirmation.";

pub(crate) const INVOKE_LOADED_TOOL_DESCRIPTION: &str =
    "Invoke a GM tool whose full schema was returned by load_tool_schema. Use this only \
after load_tool_schema has returned status=loaded_schema for the exact tool name. Pass \
name as the canonical tool name and arguments as a JSON object matching that loaded \
schema. Do not use this for tool_search, load_tool_schema, invoke_loaded_tool, or tools \
that are already visible directly.";
