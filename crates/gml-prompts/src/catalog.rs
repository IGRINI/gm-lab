use std::sync::LazyLock;

use minijinja::syntax::SyntaxConfig;
use minijinja::{AutoEscape, Environment, Error, UndefinedBehavior, Value};

pub(crate) struct PromptCatalog {
    environment: Environment<'static>,
}

impl PromptCatalog {
    fn new() -> Self {
        let mut environment = Environment::new();
        environment.set_syntax(
            SyntaxConfig::builder()
                .variable_delimiters("<<", ">>")
                .block_delimiters("<%", "%>")
                .comment_delimiters("<#", "#>")
                .build()
                .expect("valid prompt template delimiters"),
        );
        environment.set_undefined_behavior(UndefinedBehavior::Strict);
        environment.set_auto_escape_callback(|_| AutoEscape::None);
        environment.set_keep_trailing_newline(false);
        // The generated loader calls `add_template(...).expect(...)` for every
        // embedded `*.prompt.md`, so all files are parsed eagerly here. Adding a
        // file never requires updating a second registry just to validate it.
        minijinja_embed::load_templates!(&mut environment);
        Self { environment }
    }

    pub(crate) fn render(&self, name: &str, context: Value) -> Result<String, Error> {
        self.environment.get_template(name)?.render(context)
    }

    #[cfg(test)]
    pub(crate) fn template_names(&self) -> Vec<&str> {
        let mut names = self
            .environment
            .templates()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }
}

pub(crate) static PROMPT_CATALOG: LazyLock<PromptCatalog> = LazyLock::new(PromptCatalog::new);

pub(crate) fn render(name: &str, context: Value) -> Result<String, Error> {
    PROMPT_CATALOG.render(name, context)
}
