//! 响应侧工具名还原。
//!
//! 请求侧会把不满足 Kiro 约束的工具名（带 `.`、超长等）改写为 camelCase + `Hash` + 8 位 hex。
//! 模型偶尔会调用去掉 Hash 后缀或大小写/分隔符不同的名字（如 `mcpFsListDirectory`），
//! 导致客户端报 `No such tool`；正文里也可能直接复述映射后的 Hash 名。
//! 这里负责：
//! - tool_use 名称：精确映射失败时，按规范化形式唯一匹配已定义工具并还原原名；
//! - 正文 text：把出现的映射名替换回原名，确保下游永远看不到 Hash 名。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::stream::SseEvent;

const TOOL_HASH_MARKER: &str = "Hash";
const TOOL_HASH_HEX_LEN: usize = 8;

/// 去掉 `Hash` + 8 位 hex 后缀。
fn strip_hash_suffix(name: &str) -> &str {
    let Some(marker_end) = name.len().checked_sub(TOOL_HASH_HEX_LEN) else {
        return name;
    };
    if !name.is_char_boundary(marker_end) {
        return name;
    }
    let (head, hex) = name.split_at(marker_end);
    if hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        && let Some(prefix) = head.strip_suffix(TOOL_HASH_MARKER)
        && !prefix.is_empty()
    {
        return prefix;
    }
    name
}

/// 规范化：去 Hash 后缀、忽略大小写与 `_` / `-` / `.` 等非字母数字分隔符。
fn normalized_tool_name(name: &str) -> String {
    strip_hash_suffix(name)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// 响应中工具名的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolName {
    /// 请求侧发给 Kiro 的名字（schema key 映射等以它为键）。
    pub upstream: String,
    /// 返回给客户端的原始工具名。
    pub original: String,
}

/// 把上游返回的工具名解析为客户端定义的原名。
///
/// 1. 精确命中映射表或已声明工具名时直接使用；
/// 2. 否则按规范化形式在已定义工具中查找，唯一匹配时还原；
/// 3. 无法唯一确定时原样返回（交由客户端报错，而不是猜错工具）。
pub(crate) fn resolve_response_tool_name(
    tool_name_map: &HashMap<String, String>,
    known_tool_names: &HashSet<String>,
    upstream_name: &str,
) -> ResolvedToolName {
    if let Some(original) = tool_name_map.get(upstream_name) {
        return ResolvedToolName {
            upstream: upstream_name.to_string(),
            original: original.clone(),
        };
    }
    let is_original = tool_name_map
        .values()
        .any(|original| original == upstream_name);
    if is_original || known_tool_names.contains(upstream_name) {
        return ResolvedToolName {
            upstream: upstream_name.to_string(),
            original: upstream_name.to_string(),
        };
    }

    let wanted = normalized_tool_name(upstream_name);
    let mut matches: Vec<ResolvedToolName> = Vec::new();
    let mut push_unique = |candidate: ResolvedToolName| {
        if !matches.iter().any(|m| m.original == candidate.original) {
            matches.push(candidate);
        }
    };
    if !wanted.is_empty() {
        for (mapped, original) in tool_name_map {
            if normalized_tool_name(mapped) == wanted || normalized_tool_name(original) == wanted {
                push_unique(ResolvedToolName {
                    upstream: mapped.clone(),
                    original: original.clone(),
                });
            }
        }
        for name in known_tool_names {
            if tool_name_map.contains_key(name)
                || tool_name_map.values().any(|original| original == name)
            {
                continue;
            }
            if normalized_tool_name(name) == wanted {
                push_unique(ResolvedToolName {
                    upstream: name.clone(),
                    original: name.clone(),
                });
            }
        }
    }

    match matches.len() {
        1 => {
            let resolved = matches.remove(0);
            tracing::info!(
                upstream_tool_name = upstream_name,
                resolved_tool_name = %resolved.original,
                "resolved undeclared upstream tool name to a unique declared tool"
            );
            resolved
        }
        count => {
            if count > 1 {
                tracing::warn!(
                    upstream_tool_name = upstream_name,
                    candidates = count,
                    "upstream tool name matches several declared tools; leaving it unchanged"
                );
            }
            ResolvedToolName {
                upstream: upstream_name.to_string(),
                original: upstream_name.to_string(),
            }
        }
    }
}

/// 需要在正文中还原的 (映射名, 原名) 对，按映射名长度降序。
fn text_replacements(tool_name_map: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = tool_name_map
        .iter()
        .filter(|(mapped, original)| {
            mapped != original && strip_hash_suffix(mapped) != mapped.as_str()
        })
        .map(|(mapped, original)| (mapped.clone(), original.clone()))
        .collect();
    pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    pairs
}

fn replace_all<'a>(text: &'a str, pairs: &[(String, String)]) -> Cow<'a, str> {
    let mut output = Cow::Borrowed(text);
    for (mapped, original) in pairs {
        if output.contains(mapped.as_str()) {
            output = Cow::Owned(output.replace(mapped.as_str(), original));
        }
    }
    output
}

/// 非流式：还原完整文本中的 Hash 映射名。
pub(crate) fn restore_tool_names_in_text<'a>(
    tool_name_map: &HashMap<String, String>,
    text: &'a str,
) -> Cow<'a, str> {
    if tool_name_map.is_empty() || !text.contains(TOOL_HASH_MARKER) {
        return Cow::Borrowed(text);
    }
    replace_all(text, &text_replacements(tool_name_map))
}

/// 非流式：还原 content 中 text 块里的 Hash 映射名。
pub(crate) fn restore_tool_names_in_content(
    tool_name_map: &HashMap<String, String>,
    content: &mut [Value],
) {
    for block in content.iter_mut() {
        if block["type"] != "text" {
            continue;
        }
        let Some(text) = block["text"].as_str() else {
            continue;
        };
        if let Cow::Owned(restored) = restore_tool_names_in_text(tool_name_map, text) {
            block["text"] = json!(restored);
        }
    }
}

/// 流式：在 text 块中把跨 chunk 的 Hash 映射名替换为原名。
///
/// 只作用于 text 块；带签名的 thinking 不能改写，tool_use 名称走 [`resolve_response_tool_name`]。
#[derive(Debug, Default)]
pub(crate) struct ToolNameTextRestorer {
    pairs: Vec<(String, String)>,
    text_blocks: HashSet<i32>,
    pending: HashMap<i32, String>,
}

impl ToolNameTextRestorer {
    pub(crate) fn new(tool_name_map: &HashMap<String, String>) -> Option<Self> {
        let pairs = text_replacements(tool_name_map);
        (!pairs.is_empty()).then(|| Self {
            pairs,
            ..Self::default()
        })
    }

    /// 末尾可能是某个映射名前缀的最长字节数。
    fn held_suffix_len(&self, text: &str) -> usize {
        self.pairs
            .iter()
            .filter_map(|(mapped, _)| {
                (1..mapped.len())
                    .rev()
                    .find(|len| text.ends_with(&mapped[..*len]))
            })
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn apply(&mut self, events: Vec<SseEvent>) -> Vec<SseEvent> {
        let mut output = Vec::with_capacity(events.len());
        for mut event in events {
            let index = event
                .data
                .get("index")
                .and_then(Value::as_i64)
                .map(|index| index as i32);
            match (event.event.as_str(), index) {
                ("content_block_start", Some(index)) => {
                    if event.data["content_block"]["type"] == "text" {
                        self.text_blocks.insert(index);
                    }
                    output.push(event);
                }
                ("content_block_delta", Some(index))
                    if self.text_blocks.contains(&index)
                        && event.data["delta"]["type"] == "text_delta" =>
                {
                    let text = event.data["delta"]["text"].as_str().unwrap_or_default();
                    let mut combined = self.pending.remove(&index).unwrap_or_default();
                    combined.push_str(text);
                    if combined.is_empty() {
                        output.push(event);
                        continue;
                    }
                    let mut restored = replace_all(&combined, &self.pairs).into_owned();
                    let held = self.held_suffix_len(&restored);
                    if held > 0 {
                        let split = restored.len() - held;
                        self.pending.insert(index, restored[split..].to_string());
                        restored.truncate(split);
                    }
                    if !restored.is_empty() {
                        event.data["delta"]["text"] = json!(restored);
                        output.push(event);
                    }
                }
                ("content_block_stop", Some(index)) => {
                    if let Some(pending) = self.pending.remove(&index).filter(|p| !p.is_empty()) {
                        output.push(SseEvent::new(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": index,
                                "delta": { "type": "text_delta", "text": pending }
                            }),
                        ));
                    }
                    output.push(event);
                }
                _ => output.push(event),
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPED: &str = "mcpFsListDirectoryHash1a2b3c4d";
    const ORIGINAL: &str = "mcp__fs__list_directory";

    fn map() -> HashMap<String, String> {
        HashMap::from([
            (MAPPED.to_string(), ORIGINAL.to_string()),
            (
                "mcpFsReadFileHash99887766".to_string(),
                "mcp__fs__read.file".to_string(),
            ),
        ])
    }

    fn known() -> HashSet<String> {
        let mut known: HashSet<String> = map().keys().cloned().collect();
        known.extend(map().values().cloned());
        known.insert("Bash".to_string());
        known.insert("read_file".to_string());
        known
    }

    #[test]
    fn exact_mapped_and_declared_names_resolve_directly() {
        let resolved = resolve_response_tool_name(&map(), &known(), MAPPED);
        assert_eq!(resolved.original, ORIGINAL);
        assert_eq!(resolved.upstream, MAPPED);
        let resolved = resolve_response_tool_name(&map(), &known(), "Bash");
        assert_eq!(resolved.original, "Bash");
    }

    #[test]
    fn hash_less_and_case_variants_resolve_to_unique_tool() {
        for variant in [
            "mcpFsListDirectory",
            "mcp_fs_list_directory",
            "McpFsListDirectory",
            "mcpfslistdirectoryHashdeadbeef",
        ] {
            let resolved = resolve_response_tool_name(&map(), &known(), variant);
            assert_eq!(resolved.original, ORIGINAL, "{variant}");
            assert_eq!(resolved.upstream, MAPPED, "{variant}");
        }
        let resolved = resolve_response_tool_name(&map(), &known(), "bash");
        assert_eq!(resolved.original, "Bash");
    }

    #[test]
    fn unknown_or_ambiguous_names_stay_unchanged() {
        let resolved = resolve_response_tool_name(&map(), &known(), "totallyUnknown");
        assert_eq!(resolved.original, "totallyUnknown");

        // `mcpFsReadFile` 与 `read_file` 规范化后不同；构造一个真正歧义的集合。
        let mut known = known();
        known.insert("mcp_fs_read_file".to_string());
        let resolved = resolve_response_tool_name(&map(), &known, "mcpFsReadFile");
        assert_eq!(resolved.original, "mcpFsReadFile");
    }

    #[test]
    fn hash_suffix_is_only_stripped_for_real_markers() {
        assert_eq!(strip_hash_suffix(MAPPED), "mcpFsListDirectory");
        assert_eq!(strip_hash_suffix("computeHash"), "computeHash");
        assert_eq!(strip_hash_suffix("Hash1a2b3c4d"), "Hash1a2b3c4d");
        assert_eq!(strip_hash_suffix("fooHashzzzzzzzz"), "fooHashzzzzzzzz");
        assert_eq!(strip_hash_suffix("工具名称中文"), "工具名称中文");
    }

    #[test]
    fn non_stream_text_restores_hash_names() {
        let text = format!("I will call {MAPPED} now.");
        assert_eq!(
            restore_tool_names_in_text(&map(), &text),
            format!("I will call {ORIGINAL} now.")
        );
        assert!(matches!(
            restore_tool_names_in_text(&map(), "plain Hash talk"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn streaming_text_restores_hash_names_across_chunks() {
        let mut restorer = ToolNameTextRestorer::new(&map()).expect("restorer");
        let delta = |text: &str| {
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
            )
        };
        let mut out = restorer.apply(vec![
            SseEvent::new(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            delta("use mcpFsList"),
        ]);
        out.extend(restorer.apply(vec![delta("DirectoryHash1a2"), delta("b3c4d ok mcpF")]));
        out.extend(restorer.apply(vec![SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        )]));

        let text: String = out
            .iter()
            .filter_map(|event| event.data["delta"]["text"].as_str())
            .collect();
        assert_eq!(text, format!("use {ORIGINAL} ok mcpF"));
        assert!(!text.contains("Hash1a2b3c4d"));
    }
}
