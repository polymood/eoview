//! Languages of the interface. The English text in the code is the key of each text. A language is a
//! JSON file with the English text and its translation: `assets/lang/<code>.json`. A text without a
//! translation shows in English.
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Code and name of the languages. English has no file.
pub const LANGS: [(&str, &str); 2] = [("en", "English"), ("fr", "Français")];

const FILES: [&str; 1] = [include_str!("../assets/lang/fr.json")];

static TABLES: LazyLock<Vec<HashMap<String, &'static str>>> = LazyLock::new(|| {
    FILES
        .iter()
        .map(|s| {
            let m: HashMap<String, String> = serde_json::from_str(s).unwrap_or_default();
            m.into_iter().map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect()
        })
        .collect()
});

/// Index of the language in `LANGS`.
static CUR: AtomicUsize = AtomicUsize::new(0);

/// Use the language with this code. An unknown code is English.
pub fn set(code: &str) {
    CUR.store(LANGS.iter().position(|l| l.0 == code).unwrap_or(0), Ordering::Relaxed);
}

/// The code of the language of the interface.
pub fn code() -> &'static str {
    LANGS[CUR.load(Ordering::Relaxed)].0
}

/// The text `s` in the language of the interface.
pub fn t(s: &str) -> &str {
    match CUR.load(Ordering::Relaxed) {
        0 => s,
        k => TABLES.get(k - 1).and_then(|m| m.get(s).copied()).unwrap_or(s),
    }
}

/// `t(s)` with `{}` replaced by the arguments, in their order.
pub fn tf(s: &str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut parts = t(s).split("{}");
    out += parts.next().unwrap_or("");
    for (k, p) in parts.enumerate() {
        out += args.get(k).copied().unwrap_or("");
        out += p;
    }
    out
}

#[cfg(test)]
mod tests {
    /// Each text of the interface (`t("...")` and `tf("...", ...)` in the source files) has a French
    /// translation, and each translation has a text of the interface.
    #[test]
    fn all_texts_have_a_translation() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let (mut keys, mut all) = (std::collections::BTreeSet::new(), String::new());
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let src = std::fs::read_to_string(e.path()).unwrap();
            let src: String = src.lines().filter(|l| !l.trim_start().starts_with("//")).map(|l| format!("{l}\n")).collect();
            all += &src;
            for pat in ["t(\"", "tf(\""] {
                for (i, _) in src.match_indices(pat) {
                    // Only the calls: `t(` is not the end of an other name.
                    if src[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let rest = &src[i + pat.len()..];
                    let mut s = String::new();
                    let mut it = rest.chars();
                    while let Some(c) = it.next() {
                        match c {
                            '\\' => match it.next() {
                                Some('n') => s.push('\n'),
                                Some(x) => s.push(x),
                                None => {}
                            },
                            '"' => break,
                            _ => s.push(c),
                        }
                    }
                    keys.insert(s);
                }
            }
        }
        // The names of the aggregates are in the engine.
        all += &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../eo-cache/src/engine/ops.rs")).unwrap();
        let fr = &super::TABLES[0];
        let missing: Vec<&String> = keys.iter().filter(|k| !fr.contains_key(*k)).collect();
        assert!(missing.is_empty(), "{} texts without a French translation:\n{}", missing.len(), missing.iter().map(|s| format!("    {s:?}: \"\",")).collect::<Vec<_>>().join("\n"));
        // A translation of a text that the code gives to `t` in a variable (a name in a table) is not a
        // `t("...")` call: its text is in the source in quotes.
        let unused: Vec<&String> = fr.keys().filter(|k| !keys.contains(*k) && !all.contains(&format!("{k:?}"))).collect();
        assert!(unused.is_empty(), "translations of no text: {unused:?}");
        super::set("fr");
        assert_eq!(super::tf("{} / {}", &["1", "2"]), "1 / 2");
        super::set("en");
    }
}
