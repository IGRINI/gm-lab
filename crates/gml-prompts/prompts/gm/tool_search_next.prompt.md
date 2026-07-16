<% if state == "empty" -%>
Search with keywords or select:tool_name, then call load_tool_schema for one exact schema. For non-visible tools, call invoke_loaded_tool next.
<%- else -%>
Call load_tool_schema with exactly one match.name. For non-visible tools, then call invoke_loaded_tool with that name and schema-matching arguments.
<%- endif %>
