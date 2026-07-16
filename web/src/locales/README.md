# Interface locales

Each language is one folder whose name is a safe BCP-47 tag, for example
`ru`, `en`, or `pt-BR`.

To add a language:

1. Copy an existing language folder.
2. Update `meta.json`; its `code` must exactly match the folder name.
3. Translate every JSON namespace without changing keys or `{{variables}}`.
   Plural suffixes (`_zero`, `_one`, `_two`, `_few`, `_many`, `_other`) may
   follow the new language's own `Intl.PluralRules` categories.
4. Run `npm test` and `npm run build` from `web`.

Vite discovers all `locales/*/*.json` files eagerly and embeds them into the
single-file build. No registry or JavaScript import needs to be edited.

The interface language is stored only in the current browser. The response
language is a separate backend setting; both selectors use this catalog, but
changing one never changes the other.
