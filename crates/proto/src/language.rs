//! Choosing the app's language (shared by the host and the screen app). Both apps' text
//! lives in the repository's `locales` folder, one file per language.

/// The best of `available` (the locale files' names) for an OS locale: the exact one
/// ("pt-BR"), else the language ("de-AT" -> "de"), else English. Chinese is matched by
/// script: "zh-Hans-*" and mainland China to zh-CN; "zh-Hant-*", Taiwan, Hong Kong and
/// Macau to zh-TW.
pub fn pick<'a>(locale: &str, available: &[&'a str]) -> &'a str {
    let lower = locale.to_lowercase().replace('_', "-");
    let parts: Vec<&str> = lower.split('-').collect();
    let base = parts[0];
    let mut wanted = vec![lower.clone()];
    if base == "zh" {
        let traditional = parts.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"));
        wanted.push(if traditional { "zh-tw".into() } else { "zh-cn".into() });
    } else if let Some(region) = parts.last().filter(|_| parts.len() > 1) {
        wanted.push(format!("{base}-{region}"));
    }
    wanted.push(base.to_string());
    wanted.iter().find_map(|w| available.iter().find(|a| a.to_lowercase() == *w)).copied().unwrap_or("en")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_language_we_have() {
        let available = ["en", "de", "pt-BR", "zh-CN"];
        assert_eq!(pick("de-AT", &available), "de");
        assert_eq!(pick("pt_BR", &available), "pt-BR");
        assert_eq!(pick("zh-Hans-CN", &available), "zh-CN");
        assert_eq!(pick("zh-TW", &available), "en");
        assert_eq!(pick("ja-JP", &available), "en");
        assert_eq!(pick("", &available), "en");
    }
}
