use std::path::{Path, PathBuf};

fn without_final_newline(mut source: String) -> String {
    if source.ends_with('\n') {
        source.pop();
        if source.ends_with('\r') {
            source.pop();
        }
    }
    source
}

fn legacy_named_template(source: &str) -> String {
    let mut rendered = String::with_capacity(source.len());
    let mut remaining = source;

    while let Some(start) = remaining.find("<<") {
        rendered.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = expression
            .find(">>")
            .expect("embedded compatibility template has an unclosed variable");
        let name = expression[..end].trim();
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_alphanumeric()),
            "compatibility template variable must be a plain identifier: {name}"
        );
        rendered.push('{');
        rendered.push_str(name);
        rendered.push('}');
        remaining = &expression[end + 2..];
    }
    rendered.push_str(remaining);
    rendered
}

fn write_legacy_template(source: &Path, destination: &Path) {
    println!("cargo:rerun-if-changed={}", source.display());
    let source = std::fs::read_to_string(source).expect("read prompt compatibility source");
    let legacy = legacy_named_template(&without_final_newline(source));
    std::fs::write(destination, legacy).expect("write generated compatibility template");
}

fn write_without_final_newline(source: &Path, destination: &Path) {
    println!("cargo:rerun-if-changed={}", source.display());
    let source = std::fs::read_to_string(source).expect("read prompt compatibility source");
    std::fs::write(destination, without_final_newline(source))
        .expect("write generated compatibility prompt");
}

fn write_character_architect_variant(source: &Path, destination: &Path, based: bool) {
    println!("cargo:rerun-if-changed={}", source.display());
    let source = std::fs::read_to_string(source).expect("read character architect prompt");
    let (prefix, branches) = source
        .split_once("<% if based %>")
        .expect("character architect prompt has a based branch");
    let (based_text, branches) = branches
        .split_once("<% else %>")
        .expect("character architect prompt has a standalone branch");
    let (standalone_text, suffix) = branches
        .split_once("<% endif %>")
        .expect("character architect prompt closes its based branch");
    assert!(
        !suffix.contains("<% if based %>"),
        "character architect compatibility renderer supports one based branch"
    );
    let selected = if based { based_text } else { standalone_text };
    std::fs::write(
        destination,
        without_final_newline(format!("{prefix}{selected}{suffix}")),
    )
    .expect("write generated character architect compatibility prompt");
}

fn write_npc_perception_rules(source: &Path, destination: &Path) {
    println!("cargo:rerun-if-changed={}", source.display());
    let source = std::fs::read_to_string(source).expect("read NPC turn prompt");
    let (rules, _) = source
        .split_once("\n\nCURRENT SITUATION")
        .expect("NPC turn prompt contains the current-situation section");
    std::fs::write(destination, rules).expect("write generated NPC perception rules");
}

fn main() {
    minijinja_embed::embed_templates!("prompts", &[".prompt.md"]);

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    for (source, destination) in [
        ("prompts/npc/card.prompt.md", "NPC_CARD_TEMPLATE.txt"),
        (
            "prompts/npc/compact_system.prompt.md",
            "NPC_COMPACT_SYSTEM.txt",
        ),
        (
            "prompts/gm/compact_system.prompt.md",
            "GM_COMPACT_SYSTEM.txt",
        ),
    ] {
        write_legacy_template(Path::new(source), &output.join(destination));
    }
    write_without_final_newline(
        Path::new("prompts/orchestrator/visible_continuation_reminder.prompt.md"),
        &output.join("VISIBLE_CONTINUATION_REMINDER.txt"),
    );
    for (source, destination) in [
        (
            "prompts/architects/story/system.prompt.md",
            "STORY_ARCHITECT_SYSTEM.txt",
        ),
        ("prompts/rag/query_task.prompt.md", "RAG_QUERY_TASK.txt"),
    ] {
        write_without_final_newline(Path::new(source), &output.join(destination));
    }
    write_character_architect_variant(
        Path::new("prompts/architects/character/system.prompt.md"),
        &output.join("CHARACTER_ARCHITECT_SYSTEM.txt"),
        false,
    );
    write_character_architect_variant(
        Path::new("prompts/architects/character/system.prompt.md"),
        &output.join("CHARACTER_ARCHITECT_SYSTEM_BASED.txt"),
        true,
    );
    write_npc_perception_rules(
        Path::new("prompts/npc/turn_user.prompt.md"),
        &output.join("NPC_PERCEPTION_BRIEF_RULES.txt"),
    );
}
