//! Runtime localization: Fluent catalogs embedded in the binary (no install
//! paths to manage across packaging formats), language picked from the
//! desktop environment (LANG/LC_*) at startup, English as the complete
//! fallback catalog. A `HUSHMIC_LANG` environment variable overrides the
//! desktop's locale (also how the test suites pin English). Diagnostics and
//! CLI output stay English on purpose — bug reports must be readable
//! upstream.

use i18n_embed::fluent::{fluent_language_loader, FluentLanguageLoader};
use i18n_embed::unic_langid::LanguageIdentifier;
use i18n_embed::DesktopLanguageRequester;
use rust_embed::RustEmbed;
use std::sync::LazyLock;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

/// `HUSHMIC_LANG` accepts both BCP-47 ("de", "de-DE") and POSIX locale
/// spellings ("de_DE", "de_DE.UTF-8") — it is the documented knob for
/// translators to preview their work, so the spelling everyone's `$LANG`
/// uses has to work. An unparseable value warns instead of silently
/// falling through to the desktop locale.
fn override_language() -> Option<LanguageIdentifier> {
    let raw = std::env::var("HUSHMIC_LANG").ok()?;
    let parsed = parse_override(&raw);
    if parsed.is_none() {
        eprintln!("[hushmic] HUSHMIC_LANG={raw} is not a language code; using the desktop locale");
    }
    parsed
}

/// Pure spelling normalization, separated so it is testable without
/// mutating the process environment.
fn parse_override(raw: &str) -> Option<LanguageIdentifier> {
    let trimmed = raw.split(['.', '@']).next().unwrap_or("");
    trimmed.replace('_', "-").parse().ok()
}

pub static LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader = fluent_language_loader!();
    let mut requested: Vec<LanguageIdentifier> = override_language().into_iter().collect();
    requested.extend(DesktopLanguageRequester::requested_languages());
    // Negotiation failures leave the loader on the embedded English
    // fallback; a missing translation must never take the tray down.
    let _ = i18n_embed::select(&loader, &Localizations, &requested);
    // Fluent wraps placeables in Unicode isolation marks by default; they
    // surface as visible garbage in tray menus and notifications on some
    // hosts, and would break exact-label lookups in the UI tests.
    loader.set_use_isolating(false);
    loader
});

/// `tr!("key")` / `tr!("key", arg = value)`: fetch a localized message.
/// Message keys are checked against the English catalog at compile time.
/// Internal to the hushmic crate: the underlying `fl!` resolves the catalog
/// through THIS crate's `i18n.toml`, so the macro cannot be used from
/// dependent crates.
#[macro_export]
macro_rules! tr {
    ($message_id:literal) => {
        i18n_embed_fl::fl!(&*$crate::i18n::LOADER, $message_id)
    };
    ($message_id:literal, $($args:tt)*) => {
        i18n_embed_fl::fl!(&*$crate::i18n::LOADER, $message_id, $($args)*)
    };
}

/// Test-only: force the global loader to English BEFORE any message lookup.
/// All unit tests share one process, and the first `tr!` latches whatever
/// the environment says at that moment — every label-asserting test path
/// must come through here so the first toucher is always pinned. Also keeps
/// the process-env mutation in a single place.
#[cfg(test)]
pub fn pin_english() {
    std::env::set_var("HUSHMIC_LANG", "en");
    LazyLock::force(&LOADER);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_fallback_serves_exact_messages() {
        // A LOCAL loader, deliberately not the global LOADER: touching the
        // global unpinned would race the pin_english() calls other test
        // modules rely on.
        let loader = fluent_language_loader!();
        i18n_embed::select(&loader, &Localizations, &["en".parse().unwrap()])
            .expect("English catalog embeds");
        loader.set_use_isolating(false);
        assert_eq!(loader.get("tray-quit"), "Quit");
    }

    #[test]
    fn override_accepts_posix_spellings() {
        assert_eq!(
            parse_override("de_DE.UTF-8"),
            Some("de-DE".parse().unwrap())
        );
        assert_eq!(parse_override("pt_BR"), Some("pt-BR".parse().unwrap()));
        assert_eq!(parse_override("es"), Some("es".parse().unwrap()));
        assert_eq!(parse_override("ca@valencia"), Some("ca".parse().unwrap()));
        assert_eq!(parse_override("not a language"), None);
    }

    #[test]
    fn catalogs_parse_and_stay_subsets_of_english() {
        use std::collections::{BTreeMap, BTreeSet};
        let mut keys: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for path in Localizations::iter() {
            let file = Localizations::get(&path).expect("embedded file present");
            let text = std::str::from_utf8(&file.data).expect("catalogs are UTF-8");
            let resource = fluent_syntax::parser::parse(text)
                .unwrap_or_else(|(_, errors)| panic!("{path} does not parse: {errors:?}"));
            let lang = path
                .split('/')
                .next()
                .expect("catalogs live under <lang>/")
                .to_string();
            let entry = keys.entry(lang).or_default();
            for item in resource.body {
                if let fluent_syntax::ast::Entry::Message(m) = item {
                    entry.insert(m.id.name.to_string());
                }
            }
        }
        let en = keys
            .remove("en")
            .expect("English fallback catalog embedded");
        assert!(!en.is_empty(), "English catalog must define messages");
        // A translation may lag behind English (missing keys fall back),
        // but a key absent from English is unreachable: the compile-time
        // check only knows the fallback catalog.
        for (lang, lang_keys) in keys {
            let orphans: Vec<_> = lang_keys.difference(&en).collect();
            assert!(
                orphans.is_empty(),
                "{lang} has keys not present in en: {orphans:?}"
            );
        }
    }
}
