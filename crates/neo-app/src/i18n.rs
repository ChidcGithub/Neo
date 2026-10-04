//! Chinese source keys, bounded UTF-8 JSON catalogs, and allocation-free lookup.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    ZhCn,
    EnUs,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self { Self::ZhCn => "zh-CN", Self::EnUs => "en-US" }
    }

    pub fn from_code(code: &str) -> Self {
        if code.trim().eq_ignore_ascii_case("en-US") || code.trim().eq_ignore_ascii_case("en_US") {
            Self::EnUs
        } else {
            Self::ZhCn
        }
    }
}

#[cfg(not(test))]
static LANGUAGE: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
thread_local! {
    static TEST_LANGUAGE: std::cell::Cell<Language> = const { std::cell::Cell::new(Language::ZhCn) };
}

pub fn language() -> Language {
    #[cfg(not(test))]
    { if LANGUAGE.load(Ordering::Relaxed) == 1 { Language::EnUs } else { Language::ZhCn } }
    #[cfg(test)]
    { TEST_LANGUAGE.with(std::cell::Cell::get) }
}

pub fn set_language(value: Language) {
    #[cfg(not(test))]
    LANGUAGE.store(u8::from(value == Language::EnUs), Ordering::Relaxed);
    #[cfg(test)]
    TEST_LANGUAGE.with(|slot| slot.set(value));
}

/// Thread-local, nestable and panic-safe; never changes another test's language.
#[cfg(test)]
pub fn with_language<R>(value: Language, run: impl FnOnce() -> R) -> R {
    struct Guard(Language);
    impl Drop for Guard {
        fn drop(&mut self) { set_language(self.0); }
    }
    let _guard = Guard(language());
    set_language(value);
    run()
}

const CATALOG_LIMIT: usize = 512 * 1024;
const ZH_CN: &str = include_str!("../../../resources/lang/zh-CN.lang");
const EN_US: &str = include_str!("../../../resources/lang/en-US.lang");
type Catalog = BTreeMap<String, String>;
type StaticCatalog = BTreeMap<String, &'static str>;
static CATALOGS: OnceLock<[StaticCatalog; 2]> = OnceLock::new();

fn placeholder_name(name: &str) -> bool {
    let mut chars = name.bytes();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn placeholders(text: &str) -> BTreeMap<&str, usize> {
    let mut result = BTreeMap::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[..end];
        if placeholder_name(name) {
            *result.entry(name).or_default() += 1;
            rest = &rest[end + 1..];
        }
    }
    result
}

fn parse_catalog(bytes: &[u8]) -> Option<Catalog> {
    if bytes.len() > CATALOG_LIMIT { return None; }
    let catalog: Catalog = serde_json::from_slice(bytes).ok()?;
    catalog.iter().all(|(key, value)| {
        !key.trim().is_empty() && !value.trim().is_empty()
            && !key.contains('\0') && !value.contains('\0')
            && placeholders(key) == placeholders(value)
    }).then_some(catalog)
}

fn read_catalog(path: &Path) -> Option<Catalog> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > CATALOG_LIMIT as u64 { return None; }
    let mut bytes = Vec::new();
    file.take(CATALOG_LIMIT as u64 + 1).read_to_end(&mut bytes).ok()?;
    parse_catalog(&bytes)
}

// Overlay individual keys, so partial external catalogs retain embedded fallback.
// Invalid files are ignored as a whole. At most three bounded catalogs per language.
fn load_catalog(embedded: &str, source: &Path, executable: Option<&Path>) -> Catalog {
    let mut result = parse_catalog(embedded.as_bytes()).unwrap_or_default();
    if let Some(catalog) = read_catalog(source) { result.extend(catalog); }
    if let Some(catalog) = executable.and_then(read_catalog) { result.extend(catalog); }
    result
}

fn catalogs() -> &'static [StaticCatalog; 2] {
    CATALOGS.get_or_init(|| {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/lang");
        let executable = std::env::current_exe().ok()
            .and_then(|path| path.parent().map(|dir| dir.join("resources/lang")));
        [(Language::ZhCn, ZH_CN), (Language::EnUs, EN_US)].map(|(lang, embedded)| {
            let file = format!("{}.lang", lang.code());
            let external = executable.as_ref().map(|dir| dir.join(&file));
            load_catalog(embedded, &source.join(file), external.as_deref()).into_iter()
                // Only the final, bounded catalog is leaked, once for the process.
                .map(|(key, value)| (key, &*Box::leak(value.into_boxed_str()))).collect()
        })
    })
}

pub fn tr(source: &'static str) -> &'static str {
    let index = usize::from(language() == Language::EnUs);
    catalogs()[index].get(source).copied().unwrap_or(source)
}

/// Expand each named occurrence in one pass; inserted values are never scanned.
/// Unknown placeholders (and literal braces) remain untouched. First argument wins.
pub fn tf(source: &'static str, args: &[(&str, String)]) -> String {
    format_named(tr(source), args)
}

fn format_named(text: &str, args: &[(&str, String)]) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[1..end];
        if placeholder_name(name) {
            if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
                output.push_str(value);
            } else {
                output.push_str(&rest[..=end]);
            }
            rest = &rest[end + 1..];
        } else {
            output.push('{');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
#[path = "i18n_tests.rs"]
mod tests;
