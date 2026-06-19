import json

import agents
import world as world_mod
from codex_client import (
    _StreamAccumulator,
    _usage_stats,
    convert_tool_for_responses,
    strict_schema_for_responses,
    split_messages_for_responses,
)


messages = [
    {"role": "system", "content": "System A"},
    {"role": "system", "content": "System B"},
    {"role": "user", "content": "Hi"},
    {"role": "assistant", "content": "", "tool_calls": [{
        "id": "call_1",
        "type": "function",
        "function": {"name": "ask_npc", "arguments": "{\"npc_id\":\"borin\"}"},
    }]},
    {"role": "tool", "tool_call_id": "call_1", "content": "{\"ok\":true}"},
]

instructions, input_items = split_messages_for_responses(messages)
assert instructions == "System A\n\nSystem B"
assert input_items[0] == {
    "type": "message",
    "role": "user",
    "content": [{"type": "input_text", "text": "Hi"}],
}
assert input_items[1] == {
    "type": "function_call",
    "call_id": "call_1",
    "name": "ask_npc",
    "arguments": "{\"npc_id\":\"borin\"}",
}
assert input_items[2] == {
    "type": "function_call_output",
    "call_id": "call_1",
    "output": "{\"ok\":true}",
}

tool = convert_tool_for_responses({
    "type": "function",
    "function": {
        "name": "ask_npc",
        "description": "Ask NPC",
        "parameters": {"type": "object", "properties": {
            "npc_id": {"type": "string"},
            "correction": {"type": "string"},
        }, "required": ["npc_id"], "additionalProperties": False},
    },
})
assert tool == {
    "type": "function",
    "name": "ask_npc",
    "description": "Ask NPC",
    "parameters": {"type": "object", "properties": {
        "npc_id": {"type": "string"},
        "correction": {"type": ["string", "null"]},
    }, "required": ["npc_id", "correction"], "additionalProperties": False},
    "strict": True,
}

strict_nested = strict_schema_for_responses({
    "type": "object",
    "properties": {
        "exits": {"type": "array", "items": {"type": "object", "properties": {
            "id": {"type": "string"},
            "blocked_by": {"type": "string"},
        }, "required": ["id"]}},
    },
})
assert strict_nested["required"] == ["exits"]
assert strict_nested["properties"]["exits"]["items"]["additionalProperties"] is False
assert strict_nested["properties"]["exits"]["items"]["required"] == ["id", "blocked_by"]
assert strict_nested["properties"]["exits"]["items"]["properties"]["blocked_by"]["type"] == ["string", "null"]


def _assert_strict_objects(schema: dict) -> None:
    if not isinstance(schema, dict):
        return
    props = schema.get("properties")
    if isinstance(props, dict):
        assert schema["additionalProperties"] is False
        assert schema["required"] == list(props.keys())
        for child in props.values():
            _assert_strict_objects(child)
    items = schema.get("items")
    if isinstance(items, dict):
        _assert_strict_objects(items)


real_tools = {
    tool["name"]: tool
    for tool in (
        convert_tool_for_responses(raw_tool)
        for raw_tool in agents.build_gm_tools(world_mod.World())
    )
}
for real_tool in real_tools.values():
    _assert_strict_objects(real_tool["parameters"])

ask_params = real_tools["ask_npc"]["parameters"]["properties"]
assert ask_params["npc_id"]["type"] == "string"
assert ask_params["correction"]["type"] == ["string", "null"]
move_params = real_tools["move_npc"]["parameters"]["properties"]
assert move_params["present"]["type"] == "boolean"
assert move_params["visible"]["type"] == ["boolean", "null"]
assert move_params["can_hear"]["type"] == ["boolean", "null"]
where_params = real_tools["set_npc_whereabouts"]["parameters"]["properties"]
assert where_params["status"]["type"] == "string"
assert where_params["source"]["type"] == ["string", "null"]
scene_params = real_tools["set_scene"]["parameters"]["properties"]
scene_item = scene_params["items"]["items"]["properties"]
assert scene_item["name"]["type"] == "string"
assert scene_item["visible"]["type"] == ["boolean", "null"]
assert scene_item["portable"]["type"] == ["boolean", "null"]
scene_exit = scene_params["exits"]["items"]["properties"]
assert scene_exit["name"]["type"] == "string"
assert scene_exit["visible"]["type"] == ["boolean", "null"]
assert scene_exit["blocked_by"]["type"] == ["string", "null"]
roll_params = real_tools["roll_dice"]["parameters"]["properties"]
assert roll_params["target_number"]["type"] == ["integer", "null"]
assert roll_params["target_kind"]["enum"] == ["DC", "AC", "opposed_total", None]
assert roll_params["difficulty_label"]["enum"][-1] is None
assert roll_params["stakes"]["type"] == ["object", "null"]
assert roll_params["stakes"]["properties"]["intent"]["type"] == ["string", "null"]
search_params = real_tools["tool_search"]["parameters"]["properties"]
assert search_params["max_results"]["type"] == ["integer", "null"]

acc = _StreamAccumulator()
list(acc.handle({"type": "response.output_text.delta", "delta": "A"}))
list(acc.handle({
    "type": "response.output_item.added",
    "output_index": 1,
    "item": {"type": "function_call", "id": "fc_1", "call_id": "call_2", "name": "roll_dice"},
}))
list(acc.handle({
    "type": "response.function_call_arguments.delta",
    "output_index": 1,
    "item_id": "fc_1",
    "delta": "{\"notation\":\"1d20\"}",
}))
list(acc.handle({
    "type": "response.completed",
    "response": {
        "id": "resp_1",
        "usage": {
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 70},
            "output_tokens": 8,
            "total_tokens": 108,
        },
    },
}))
result = acc.finish(123.0)
assert result.content == "A"
assert result.calls == [{"id": "call_2", "name": "roll_dice", "arguments": {"notation": "1d20"}}]
assert result.usage["input_tokens_details"]["cached_tokens"] == 70

stats = _usage_stats(result.usage, 123.0)
assert stats["prompt_eval_count"] == 100
assert stats["eval_count"] == 8
assert stats["cached_tokens"] == 70

print("CODEX ADAPTER TESTS PASSED")
