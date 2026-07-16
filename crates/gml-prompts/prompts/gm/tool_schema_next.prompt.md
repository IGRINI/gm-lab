<% if status == "invalid" -%>
Pass one exact canonical tool name returned by tool_search.
<%- elif status == "missing" -%>
Call tool_search again with keywords or select:tool_name.
<%- elif status == "already_loaded" -%>
The schema is already visible; call the tool directly when needed.
<%- else -%>
Call invoke_loaded_tool with this exact name and arguments matching the returned schema.
<%- endif %>
