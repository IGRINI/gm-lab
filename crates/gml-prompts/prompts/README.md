# Prompt templates

All model-facing prompt text in this crate uses Markdown files named
`*.prompt.md`. The build embeds every matching file below this directory into
one MiniJinja catalog.

Template syntax:

- `<< value >>` inserts a value;
- `<% if condition %> ... <% else %> ... <% endif %>` controls optional blocks;
- `<% include "path/to/partial.prompt.md" %>` includes another template;
- `<# comment #>` adds a template-only comment.

The nonstandard delimiters intentionally leave JSON objects and examples
untouched. Rendering is strict: callers must provide every referenced value,
including explicit boolean flags used by conditions. Values are inserted
without HTML escaping because the output is model input, not a web document.
Every production template also has a typed `PromptId`; a catalog test rejects
both missing files and orphan files that no caller can address safely.

Directory names follow the model role that consumes the prompt:

- `gm/` and `npc/` contain live-game prompts;
- `architects/`, `generators/`, and `seed/` contain authoring flows;
- `orchestrator/` contains model-facing tool and memory policy;
- `rag/` contains embedding instructions;
- `shared/` contains cross-role fragments.

Keep message roles, ordering, state serialization, provider payloads, and tool
argument schemas in Rust. Templates own only model-facing prose and its local
conditional assembly. In particular, do not merge dynamic world state or chat
history into `gm/system.prompt.md`: the static system message must remain the
first byte-stable cache prefix.
The raw cache-prefix files `gm/system.prompt.md` and `npc/system.prompt.md` are
therefore literal-only and must not contain template delimiters.

Structured tool-result data, runtime validation errors, and localized UI text
remain in their owning Rust or localization layer. Reusable instructions that
tell a model what to do next belong here.

Every new template must have a render test for all conditional branches. A
missing variable is an error; do not rely on undefined values being false.
