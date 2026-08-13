pub fn normalize_model_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

pub fn normalize_supported_models(models: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for model in models {
        let Some(normalized) = normalize_model_id(&model) else {
            continue;
        };
        if out.iter().any(|existing| existing == &normalized) {
            continue;
        }
        out.push(normalized);
    }
    out
}

pub fn expand_claude_supported_model_variants(
    models: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for model in models {
        let Some(model) = normalize_model_id(&model) else {
            continue;
        };
        if !model.starts_with("claude-") {
            continue;
        }
        for variant in claude_model_variants(&model) {
            push_unique_model(&mut out, variant);
        }
    }
    out
}

fn push_unique_model(out: &mut Vec<String>, model: String) {
    if model.trim().is_empty() {
        return;
    }
    if out.iter().any(|existing| existing == &model) {
        return;
    }
    out.push(model);
}

fn claude_model_variants(model: &str) -> Vec<String> {
    let (base, suffix) = split_supported_model_suffix(model);
    let mut variants = Vec::new();
    push_unique_model(&mut variants, apply_supported_model_suffix(&base, suffix));

    for base_variant in claude_base_model_variants(&base) {
        push_unique_model(
            &mut variants,
            apply_supported_model_suffix(&base_variant, suffix),
        );
    }
    variants
}

fn split_supported_model_suffix(model: &str) -> (String, Option<&'static str>) {
    if let Some(base) = model.strip_suffix("-thinking") {
        return (base.to_string(), Some("-thinking"));
    }
    (model.to_string(), None)
}

fn apply_supported_model_suffix(model: &str, suffix: Option<&str>) -> String {
    match suffix {
        Some(suffix) => format!("{model}{suffix}"),
        None => model.to_string(),
    }
}

fn claude_base_model_variants(model: &str) -> Vec<String> {
    let parts = model.split('-').collect::<Vec<_>>();
    let mut out = Vec::new();

    if let Some((family, major, minor, date)) = parse_modern_claude_model(&parts) {
        let dot = minor.map(|minor| format!("claude-{family}-{major}.{minor}"));
        let dash = minor.map(|minor| format!("claude-{family}-{major}-{minor}"));
        let base = minor
            .map(|minor| format!("claude-{family}-{major}-{minor}"))
            .unwrap_or_else(|| format!("claude-{family}-{major}"));

        if let Some(dot) = dot.as_deref() {
            push_unique_model(&mut out, dot.to_string());
        }
        if let Some(dash) = dash.as_deref() {
            push_unique_model(&mut out, dash.to_string());
        }
        push_unique_model(&mut out, base.clone());

        if let Some(date) = date {
            let dated = minor
                .map(|minor| format!("claude-{family}-{major}-{minor}-{date}"))
                .unwrap_or_else(|| format!("claude-{family}-{major}-{date}"));
            push_unique_model(&mut out, dated);
        }
        if let Some(known_dated) = known_anthropic_dated_model(&base) {
            push_unique_model(&mut out, known_dated.to_string());
        }
        return out;
    }

    if let Some((family, date)) = parse_legacy_claude_35_model(&parts) {
        push_unique_model(&mut out, format!("claude-3-5-{family}"));
        push_unique_model(&mut out, format!("claude-3.5-{family}"));
        if let Some(date) = date {
            push_unique_model(&mut out, format!("claude-3-5-{family}-{date}"));
        }
        if let Some(known_dated) = known_anthropic_dated_model(&format!("claude-3-5-{family}")) {
            push_unique_model(&mut out, known_dated.to_string());
        }
    }

    out
}

fn parse_modern_claude_model<'a>(
    parts: &'a [&str],
) -> Option<(&'a str, &'a str, Option<&'a str>, Option<&'a str>)> {
    if parts.len() < 3 || parts.first().copied() != Some("claude") {
        return None;
    }
    let family = parts.get(1).copied()?;
    if !family.chars().all(|ch| ch.is_ascii_lowercase()) {
        return None;
    }
    let version = parts.get(2).copied()?;
    if let Some((major, minor)) = version.split_once('.') {
        if major.chars().all(|ch| ch.is_ascii_digit())
            && minor.chars().all(|ch| ch.is_ascii_digit())
        {
            return Some((family, major, Some(minor), None));
        }
        return None;
    }
    if !version.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    match parts.len() {
        3 => Some((family, version, None, None)),
        4 => {
            let next = parts[3];
            if is_yyyymmdd(next) {
                Some((family, version, None, Some(next)))
            } else if next.chars().all(|ch| ch.is_ascii_digit()) {
                Some((family, version, Some(next), None))
            } else {
                None
            }
        }
        _ => {
            let minor = parts[3];
            let date = parts[4];
            if minor.chars().all(|ch| ch.is_ascii_digit()) && is_yyyymmdd(date) {
                Some((family, version, Some(minor), Some(date)))
            } else {
                None
            }
        }
    }
}

fn parse_legacy_claude_35_model<'a>(parts: &'a [&str]) -> Option<(&'a str, Option<&'a str>)> {
    if parts.len() < 3 || parts.first().copied() != Some("claude") {
        return None;
    }
    if parts.get(1).copied() == Some("3.5") {
        let family = parts.get(2).copied()?;
        if matches!(family, "sonnet" | "haiku") {
            return Some((family, None));
        }
    }
    if parts.get(1).copied() == Some("3") && parts.get(2).copied() == Some("5") {
        let family = parts.get(3).copied()?;
        if matches!(family, "sonnet" | "haiku") {
            let date = parts.get(4).copied().filter(|value| is_yyyymmdd(value));
            return Some((family, date));
        }
    }
    None
}

fn is_yyyymmdd(value: &str) -> bool {
    value.len() == 8 && value.chars().all(|ch| ch.is_ascii_digit())
}

fn known_anthropic_dated_model(model: &str) -> Option<&'static str> {
    match model {
        "claude-sonnet-4" => Some("claude-sonnet-4-20250514"),
        "claude-opus-4" => Some("claude-opus-4-20250514"),
        "claude-sonnet-4-5" => Some("claude-sonnet-4-5-20250929"),
        "claude-opus-4-5" => Some("claude-opus-4-5-20251101"),
        "claude-haiku-4-5" => Some("claude-haiku-4-5-20251001"),
        "claude-3-5-sonnet" => Some("claude-3-5-sonnet-20241022"),
        "claude-3-5-haiku" => Some("claude-3-5-haiku-20241022"),
        _ => None,
    }
}

pub fn model_is_supported_by_list(models: &[String], candidates: &[Option<&str>]) -> bool {
    if models.is_empty() {
        return true;
    }

    let supported_models = normalize_supported_models(models.iter().cloned());
    if supported_models.is_empty() {
        return false;
    }

    for candidate in candidates.iter().flatten() {
        let Some(model) = normalize_model_id(candidate) else {
            continue;
        };
        if supported_models.iter().any(|supported| supported == &model) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_models() {
        assert_eq!(
            normalize_supported_models(vec![
                " Claude-Sonnet-4 ".to_string(),
                "".to_string(),
                "claude-sonnet-4".to_string(),
                "claude-haiku-4".to_string(),
            ]),
            vec!["claude-sonnet-4", "claude-haiku-4"]
        );
    }

    #[test]
    fn expands_kiro_claude_models_to_common_official_request_forms() {
        assert_eq!(
            expand_claude_supported_model_variants(vec![
                "claude-sonnet-4.5".to_string(),
                "claude-opus-4.8".to_string(),
                "auto".to_string(),
            ]),
            vec![
                "claude-sonnet-4.5",
                "claude-sonnet-4-5",
                "claude-sonnet-4-5-20250929",
                "claude-opus-4.8",
                "claude-opus-4-8",
            ]
        );
    }

    #[test]
    fn expands_dated_claude_models_back_to_short_forms() {
        assert_eq!(
            expand_claude_supported_model_variants(vec![
                "claude-sonnet-4-5-20250929".to_string(),
                "claude-3-5-haiku-20241022".to_string(),
            ]),
            vec![
                "claude-sonnet-4-5-20250929",
                "claude-sonnet-4.5",
                "claude-sonnet-4-5",
                "claude-3-5-haiku-20241022",
                "claude-3-5-haiku",
                "claude-3.5-haiku",
            ]
        );
    }

    #[test]
    fn expands_thinking_suffix_without_family_fallback() {
        assert_eq!(
            expand_claude_supported_model_variants(vec![
                "claude-opus-4.8-thinking".to_string(),
                "claude-sonnet-5".to_string(),
            ]),
            vec![
                "claude-opus-4.8-thinking",
                "claude-opus-4-8-thinking",
                "claude-sonnet-5",
            ]
        );
    }

    #[test]
    fn empty_supported_models_allows_any_model() {
        assert!(model_is_supported_by_list(&[], &[Some("anything")]));
    }

    #[test]
    fn supported_models_exact_match_any_candidate() {
        let models = vec![" Claude-Sonnet-4 ".to_string()];
        assert!(model_is_supported_by_list(
            &models,
            &[Some("client-alias"), Some("Claude-Sonnet-4")]
        ));
        assert!(!model_is_supported_by_list(
            &models,
            &[Some("claude-haiku-4"), None]
        ));
    }

    #[test]
    fn supported_models_do_not_match_version_equivalent_aliases() {
        assert!(!model_is_supported_by_list(
            &["claude-opus-4.8".to_string()],
            &[Some("claude-opus-4-8")]
        ));
        assert!(!model_is_supported_by_list(
            &["claude-opus-4-8".to_string()],
            &[Some("claude-opus-4.8")]
        ));
    }

    #[test]
    fn supported_models_do_not_match_explicit_anthropic_date_aliases() {
        assert!(!model_is_supported_by_list(
            &["claude-sonnet-4-20250514".to_string()],
            &[Some("claude-sonnet-4")]
        ));
        assert!(!model_is_supported_by_list(
            &["claude-sonnet-4".to_string()],
            &[Some("claude-sonnet-4-20250514")]
        ));
    }

    #[test]
    fn supported_models_do_not_use_family_fallback() {
        assert!(!model_is_supported_by_list(
            &["claude-sonnet-4.6".to_string()],
            &[Some("claude-sonnet-4.5")]
        ));
        assert!(!model_is_supported_by_list(
            &["claude-opus-4.8".to_string()],
            &[Some("claude-sonnet-4.6")]
        ));
    }
}
