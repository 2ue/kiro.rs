use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

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
        return out;
    }

    if let Some((family, date)) = parse_legacy_claude_35_model(&parts) {
        push_unique_model(&mut out, format!("claude-3-5-{family}"));
        push_unique_model(&mut out, format!("claude-3.5-{family}"));
        if let Some(date) = date {
            push_unique_model(&mut out, format!("claude-3-5-{family}-{date}"));
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

/// `[1m]` selects the 1M context window of the same model, so it never changes which
/// credentials can serve it. Handles both `x[1m]` and `x[1m]-thinking`.
fn strip_context_window_suffix(model: String) -> String {
    if let Some(base) = model.strip_suffix("[1m]") {
        return base.to_string();
    }
    if let Some(base) = model
        .strip_suffix("-thinking")
        .and_then(|base| base.strip_suffix("[1m]"))
    {
        return format!("{base}-thinking");
    }
    model
}

fn support_match_variants(model: String) -> Vec<String> {
    let model = strip_context_window_suffix(model);
    let mut variants = expand_claude_supported_model_variants([model.clone()]);
    if let Some(base) = model.strip_suffix("-thinking") {
        variants.extend(expand_claude_supported_model_variants([base.to_string()]));
    } else {
        variants.extend(expand_claude_supported_model_variants([format!(
            "{model}-thinking"
        )]));
    }
    if variants.is_empty() {
        variants.push(model);
    }
    variants.sort();
    variants.dedup();
    variants
}

pub fn model_is_supported_by_list(models: &[String], candidates: &[Option<&str>]) -> bool {
    if models.is_empty() {
        return true;
    }
    // 不带模型的请求（WebSearch MCP 等辅助调用）不受模型列表限制；否则账号一旦有
    // 自身或订阅等级模型列表，就再也选不中它，表现为 websearch_mcp_scheduler_unavailable。
    if candidates
        .iter()
        .flatten()
        .all(|candidate| candidate.trim().is_empty())
    {
        return true;
    }

    MODEL_SUPPORT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let supported = cache.supported_variants(models);
        if supported.is_empty() {
            return false;
        }
        candidates.iter().flatten().any(|candidate| {
            let candidate = candidate.trim();
            !candidate.is_empty()
                && cache
                    .candidate_variants(candidate)
                    .iter()
                    .any(|variant| supported.contains(variant))
        })
    })
}

// Scheduler scans ask every account whether it serves the request model, several times per
// request, and most accounts share one subscription-tier list. Expanding that list into match
// variants on every call allocated hundreds of strings per account; with ~900 accounts it was
// over half the process CPU, all of it under the scheduler's `entries` lock. The expansions are
// pure functions of the list and candidate text, so they are cached by content: a hit compares
// the stored list with the given one, so a changed list never reuses a stale expansion.
thread_local! {
    static MODEL_SUPPORT_CACHE: RefCell<ModelSupportCache> =
        RefCell::new(ModelSupportCache::default());
}

/// Distinct lists or candidates kept per thread before the cache starts over.
const MODEL_SUPPORT_CACHE_MAX_ENTRIES: usize = 256;

#[derive(Default)]
struct ModelSupportCache {
    /// The list of the previous lookup: consecutive accounts usually share it, so this skips
    /// hashing the list again.
    last: Option<Rc<CachedModelList>>,
    lists: HashMap<u64, Vec<Rc<CachedModelList>>>,
    candidates: HashMap<String, Rc<[String]>>,
}

struct CachedModelList {
    models: Vec<String>,
    variants: HashSet<String>,
}

impl ModelSupportCache {
    fn supported_variants(&mut self, models: &[String]) -> Rc<CachedModelList> {
        if let Some(last) = self.last.as_ref().filter(|last| last.models == models) {
            return last.clone();
        }
        let key = {
            let mut hasher = DefaultHasher::new();
            models.hash(&mut hasher);
            hasher.finish()
        };
        let cached = self
            .lists
            .get(&key)
            .and_then(|bucket| bucket.iter().find(|list| list.models == models))
            .cloned();
        let list = cached.unwrap_or_else(|| {
            if self.lists.len() >= MODEL_SUPPORT_CACHE_MAX_ENTRIES {
                self.lists.clear();
            }
            let list = Rc::new(CachedModelList {
                models: models.to_vec(),
                variants: normalize_supported_models(models.iter().cloned())
                    .into_iter()
                    .flat_map(support_match_variants)
                    .collect(),
            });
            self.lists.entry(key).or_default().push(list.clone());
            list
        });
        self.last = Some(list.clone());
        list
    }

    /// `candidate` is already trimmed and not empty.
    fn candidate_variants(&mut self, candidate: &str) -> Rc<[String]> {
        if let Some(variants) = self.candidates.get(candidate) {
            return variants.clone();
        }
        if self.candidates.len() >= MODEL_SUPPORT_CACHE_MAX_ENTRIES {
            self.candidates.clear();
        }
        let variants: Rc<[String]> = support_match_variants(candidate.to_ascii_lowercase()).into();
        self.candidates
            .insert(candidate.to_string(), variants.clone());
        variants
    }
}

impl CachedModelList {
    fn is_empty(&self) -> bool {
        self.variants.is_empty()
    }

    fn contains(&self, variant: &str) -> bool {
        self.variants.contains(variant)
    }
}

/// Subscription tier of a Kiro subscription title, the same keys the account page uses:
/// `free`, `students`, `pro`, `pro_plus`, `pro_max`, `power`, or `unknown`.
pub fn subscription_tier_key(title: Option<&str>) -> &'static str {
    let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) else {
        return "unknown";
    };
    // Scheduler scans resolve the tier of every account, and accounts share a handful of
    // titles: remember the last title instead of lowercasing it again for each account.
    LAST_SUBSCRIPTION_TIER.with(|last| {
        let mut last = last.borrow_mut();
        if let Some((last_title, tier)) = last.as_ref()
            && last_title == title
        {
            return *tier;
        }
        let tier = subscription_tier_key_uncached(title);
        *last = Some((title.to_string(), tier));
        tier
    })
}

thread_local! {
    static LAST_SUBSCRIPTION_TIER: RefCell<Option<(String, &'static str)>> =
        const { RefCell::new(None) };
}

fn subscription_tier_key_uncached(title: &str) -> &'static str {
    let lower = title.to_lowercase();
    let compact: String = lower
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if compact.contains("student") {
        "students"
    } else if compact.contains("power") {
        "power"
    } else if compact.contains("promax") {
        "pro_max"
    } else if lower.contains("pro+")
        || lower.contains("pro plus")
        || lower.contains("pro_plus")
        || lower.contains("pro-plus")
        || compact.contains("proplus")
    {
        "pro_plus"
    } else if lower.contains("trial") || lower.contains("试用") {
        // Kiro no longer publishes Trial as a tier; legacy trial labels are Free.
        "free"
    } else if lower.contains("free") || lower.contains("免费") {
        "free"
    } else if lower.contains("pro") {
        "pro"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subscription_tier_key_matches_kiro_titles() {
        for (title, tier) in [
            (Some("KIRO FREE"), "free"),
            (Some("Kiro Free Trial"), "free"),
            (Some("KIRO PRO"), "pro"),
            (Some("KIRO PRO+"), "pro_plus"),
            (Some("KIRO PRO MAX"), "pro_max"),
            (Some("KIRO POWER"), "power"),
            (Some("Kiro Students"), "students"),
            (Some(""), "unknown"),
            (None, "unknown"),
        ] {
            assert_eq!(super::subscription_tier_key(title), tier, "{title:?}");
        }
    }

    use super::*;

    #[test]
    fn thinking_and_1m_suffixes_schedule_on_the_base_model_catalog() {
        let catalog = vec![
            "claude-sonnet-4.6".to_string(),
            "claude-sonnet-5".to_string(),
            "claude-sonnet-5.5".to_string(),
            "claude-opus-4.8".to_string(),
            "claude-opus-5.5".to_string(),
            "claude-fable-5".to_string(),
            "claude-fable-5.1".to_string(),
            "claude-haiku-5.5".to_string(),
            "claude-haiku-4-5-20251001".to_string(),
        ];
        for candidate in [
            "claude-sonnet-4-6-thinking",
            "claude-sonnet-4.6-thinking",
            "claude-sonnet-4-6[1m]",
            "claude-sonnet-4-6-thinking[1m]",
            "claude-sonnet-4-6[1m]-thinking",
            "claude-sonnet-5-thinking",
            "claude-sonnet-5[1m]",
            "claude-sonnet-5-5-thinking",
            "claude-sonnet-5.5[1m]-thinking",
            "claude-opus-4-8-thinking",
            "claude-opus-4.8[1m]",
            "claude-opus-5-5-thinking",
            "claude-opus-5.5-thinking[1m]",
            "claude-fable-5-thinking",
            "claude-fable-5[1m]-thinking",
            "claude-fable-5-1-thinking",
            "claude-fable-5.1[1m]-thinking",
            "claude-haiku-5-5-thinking",
            "claude-haiku-5.5[1m]-thinking",
            "claude-haiku-4-5-thinking",
            "claude-haiku-4-5-20251001-thinking",
            "claude-haiku-4.5[1m]",
        ] {
            assert!(
                model_is_supported_by_list(&catalog, &[Some(candidate)]),
                "{candidate} should match the base model catalog"
            );
        }
        for candidate in ["claude-opus-4-7-thinking", "claude-sonnet-4-5[1m]"] {
            assert!(!model_is_supported_by_list(&catalog, &[Some(candidate)]));
        }
    }

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
        let listed = vec!["claude-haiku-4.5".to_string()];
        assert!(model_is_supported_by_list(&listed, &[None]));
        assert!(model_is_supported_by_list(&listed, &[Some("  ")]));
        assert!(!model_is_supported_by_list(
            &listed,
            &[Some("claude-opus-4.8")]
        ));
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
    fn supported_models_match_version_equivalent_claude_aliases() {
        assert!(model_is_supported_by_list(
            &["claude-opus-4.8".to_string()],
            &[Some("claude-opus-4-8")]
        ));
        assert!(model_is_supported_by_list(
            &["claude-opus-4-8".to_string()],
            &[Some("claude-opus-4.8")]
        ));
    }

    #[test]
    fn supported_models_match_explicit_anthropic_date_aliases() {
        assert!(model_is_supported_by_list(
            &["claude-sonnet-4-20250514".to_string()],
            &[Some("claude-sonnet-4")]
        ));
        assert!(model_is_supported_by_list(
            &["claude-sonnet-4".to_string()],
            &[Some("claude-sonnet-4-20250514")]
        ));
    }

    #[test]
    fn supported_models_match_thinking_and_dated_sonnet_haiku_aliases() {
        assert!(model_is_supported_by_list(
            &["claude-sonnet-4-5-20250929".to_string()],
            &[Some("claude-sonnet-4.5-thinking")]
        ));
        assert!(model_is_supported_by_list(
            &["claude-haiku-4.5-thinking".to_string()],
            &[Some("claude-haiku-4-5-20251001")]
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

    /// The match as computed before the cache: expand both sides on every call.
    fn uncached_model_is_supported_by_list(models: &[String], candidates: &[Option<&str>]) -> bool {
        if models.is_empty() {
            return true;
        }
        if candidates
            .iter()
            .flatten()
            .all(|candidate| normalize_model_id(candidate).is_none())
        {
            return true;
        }
        let supported = normalize_supported_models(models.iter().cloned())
            .into_iter()
            .flat_map(support_match_variants)
            .collect::<Vec<_>>();
        if supported.is_empty() {
            return false;
        }
        candidates.iter().flatten().any(|candidate| {
            normalize_model_id(candidate).is_some_and(|model| {
                support_match_variants(model)
                    .iter()
                    .any(|variant| supported.contains(variant))
            })
        })
    }

    #[test]
    fn cached_support_match_agrees_with_expanding_on_every_call() {
        let lists: Vec<Vec<String>> = [
            vec![],
            vec!["  ", ""],
            vec![
                "auto",
                "claude-haiku-4.5",
                "claude-sonnet-4-5-20250929",
                "deepseek-3.2",
            ],
            vec![
                "Claude-Opus-4.8",
                "claude-opus-4-8",
                "claude-3-5-haiku-20241022",
            ],
            vec!["claude-sonnet-4.6-thinking", "glm-5", "claude-fable-5.1"],
        ]
        .into_iter()
        .map(|list| list.into_iter().map(str::to_string).collect())
        .collect();
        let candidates: [&[Option<&str>]; 9] = [
            &[],
            &[None],
            &[Some("  ")],
            &[Some("claude-haiku-4-5")],
            &[Some(" CLAUDE-HAIKU-4.5 ")],
            &[None, Some("claude-opus-4.8-thinking")],
            &[Some("claude-3.5-haiku"), Some("glm-5")],
            &[Some("claude-sonnet-4-6[1m]")],
            &[Some("deepseek-3.2"), Some("claude-opus-5")],
        ];
        // Two rounds: the second one answers from the cache.
        for _ in 0..2 {
            for list in &lists {
                for candidate in candidates {
                    assert_eq!(
                        model_is_supported_by_list(list, candidate),
                        uncached_model_is_supported_by_list(list, candidate),
                        "{list:?} {candidate:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn cached_support_match_follows_list_changes() {
        let mut list = vec![
            "claude-haiku-4.5".to_string(),
            "claude-sonnet-4.5".to_string(),
        ];
        assert!(model_is_supported_by_list(
            &list,
            &[Some("claude-sonnet-4-5")]
        ));
        // Same allocation, different content: must not reuse the previous expansion.
        list[1] = "claude-opus-4.8".to_string();
        assert!(!model_is_supported_by_list(
            &list,
            &[Some("claude-sonnet-4-5")]
        ));
        assert!(model_is_supported_by_list(
            &list,
            &[Some("claude-opus-4-8")]
        ));
        // Switching between lists keeps each answer.
        let other = vec!["claude-sonnet-4.5".to_string()];
        for _ in 0..3 {
            assert!(model_is_supported_by_list(
                &other,
                &[Some("claude-sonnet-4-5")]
            ));
            assert!(!model_is_supported_by_list(
                &list,
                &[Some("claude-sonnet-4-5")]
            ));
        }
    }

    #[test]
    fn subscription_tier_key_cache_follows_title_changes() {
        for _ in 0..2 {
            assert_eq!(subscription_tier_key(Some("KIRO FREE")), "free");
            assert_eq!(subscription_tier_key(Some(" KIRO FREE ")), "free");
            assert_eq!(subscription_tier_key(Some("KIRO POWER")), "power");
            assert_eq!(subscription_tier_key(Some("KIRO PRO+")), "pro_plus");
            assert_eq!(subscription_tier_key(Some("KIRO PRO")), "pro");
        }
    }
}
