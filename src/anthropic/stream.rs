//! 流式响应处理模块
//!
//! 实现 Kiro → Anthropic 流式响应转换和 SSE 状态管理

use std::collections::{HashMap, HashSet};

use serde_json::json;
use uuid::Uuid;

use crate::kiro::model::events::{Event, MetadataTokenUsage};
use crate::model::config::PromptCacheSimulationMode;

use super::envelope;

/// 找到小于等于目标位置的最近有效UTF-8字符边界
///
/// UTF-8字符可能占用1-4个字节，直接按字节位置切片可能会切在多字节字符中间导致panic。
/// 这个函数从目标位置向前搜索，找到最近的有效字符边界。
fn find_char_boundary(s: &str, target: usize) -> usize {
    if target >= s.len() {
        return s.len();
    }
    if target == 0 {
        return 0;
    }
    // 从目标位置向前搜索有效的字符边界
    let mut pos = target;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// 需要跳过的包裹字符
///
/// 当 thinking 标签被这些字符包裹时，认为是在引用标签而非真正的标签：
/// - 反引号 (`)：行内代码
/// - 双引号 (")：字符串
/// - 单引号 (')：字符串
const QUOTE_CHARS: &[u8] = &[
    b'`', b'"', b'\'', b'\\', b'#', b'!', b'@', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'-',
    b'_', b'=', b'+', b'[', b']', b'{', b'}', b';', b':', b'<', b'>', b',', b'.', b'?', b'/',
];

/// 检查指定位置的字符是否是引用字符
fn is_quote_char(buffer: &str, pos: usize) -> bool {
    buffer
        .as_bytes()
        .get(pos)
        .map(|c| QUOTE_CHARS.contains(c))
        .unwrap_or(false)
}

/// 查找真正的 thinking 结束标签（不被引用字符包裹，且后面有双换行符）
///
/// 当模型在思考过程中提到 `</thinking>` 时，通常会用反引号、引号等包裹，
/// 或者在同一行有其他内容（如"关于 </thinking> 标签"）。
/// 这个函数会跳过这些情况，只返回真正的结束标签位置。
///
/// 跳过的情况：
/// - 被引用字符包裹（反引号、引号等）
/// - 后面没有双换行符（真正的结束标签后面会有 `\n\n`）
/// - 标签在缓冲区末尾（流式处理时需要等待更多内容）
///
/// # 参数
/// - `buffer`: 要搜索的字符串
///
/// # 返回值
/// - `Some(pos)`: 真正的结束标签的起始位置
/// - `None`: 没有找到真正的结束标签
fn find_real_thinking_end_tag(buffer: &str) -> Option<usize> {
    const TAG: &str = "</thinking>";
    let mut search_start = 0;

    while let Some(pos) = buffer[search_start..].find(TAG) {
        let absolute_pos = search_start + pos;

        // 检查前面是否有引用字符
        let has_quote_before = absolute_pos > 0 && is_quote_char(buffer, absolute_pos - 1);

        // 检查后面是否有引用字符
        let after_pos = absolute_pos + TAG.len();
        let has_quote_after = is_quote_char(buffer, after_pos);

        // 如果被引用字符包裹，跳过
        if has_quote_before || has_quote_after {
            search_start = absolute_pos + 1;
            continue;
        }

        // 检查后面的内容
        let after_content = &buffer[after_pos..];

        // 如果标签后面内容不足以判断是否有双换行符，等待更多内容
        if after_content.len() < 2 {
            return None;
        }

        // 真正的 thinking 结束标签后面会有双换行符 `\n\n`
        if after_content.starts_with("\n\n") {
            return Some(absolute_pos);
        }

        // 不是双换行符，跳过继续搜索
        search_start = absolute_pos + 1;
    }

    None
}

/// 查找缓冲区末尾的 thinking 结束标签（允许末尾只有空白字符）
///
/// 用于“边界事件”场景：例如 thinking 结束后立刻进入 tool_use，或流结束，
/// 此时 `</thinking>` 后面可能没有 `\n\n`，但结束标签依然应被识别并过滤。
///
/// 约束：只有当 `</thinking>` 之后全部都是空白字符时才认为是结束标签，
/// 以避免在 thinking 内容中提到 `</thinking>`（非结束标签）时误判。
fn find_real_thinking_end_tag_at_buffer_end(buffer: &str) -> Option<usize> {
    const TAG: &str = "</thinking>";
    let mut search_start = 0;

    while let Some(pos) = buffer[search_start..].find(TAG) {
        let absolute_pos = search_start + pos;

        // 检查前面是否有引用字符
        let has_quote_before = absolute_pos > 0 && is_quote_char(buffer, absolute_pos - 1);

        // 检查后面是否有引用字符
        let after_pos = absolute_pos + TAG.len();
        let has_quote_after = is_quote_char(buffer, after_pos);

        if has_quote_before || has_quote_after {
            search_start = absolute_pos + 1;
            continue;
        }

        // 只有当标签后面全部是空白字符时才认定为结束标签
        if buffer[after_pos..].trim().is_empty() {
            return Some(absolute_pos);
        }

        search_start = absolute_pos + 1;
    }

    None
}

/// 查找真正的 thinking 开始标签（不被引用字符包裹）
///
/// 与 `find_real_thinking_end_tag` 类似，跳过被引用字符包裹的开始标签。
fn find_real_thinking_start_tag(buffer: &str) -> Option<usize> {
    const TAG: &str = "<thinking>";
    let mut search_start = 0;

    while let Some(pos) = buffer[search_start..].find(TAG) {
        let absolute_pos = search_start + pos;

        // 检查前面是否有引用字符
        let has_quote_before = absolute_pos > 0 && is_quote_char(buffer, absolute_pos - 1);

        // 检查后面是否有引用字符
        let after_pos = absolute_pos + TAG.len();
        let has_quote_after = is_quote_char(buffer, after_pos);

        // 如果不被引用字符包裹，则是真正的开始标签
        if !has_quote_before && !has_quote_after {
            return Some(absolute_pos);
        }

        // 继续搜索下一个匹配
        search_start = absolute_pos + 1;
    }

    None
}

/// 检查 `name_pos`（指向标签名首字母）的前面是否构成合法的开标签起始，
/// 兼容裸写法 `<tag` 和带命名空间前缀的写法 `<prefix:tag`。
fn open_tag_lt_pos(buffer: &str, name_pos: usize) -> Option<usize> {
    let bytes = buffer.as_bytes();
    if name_pos == 0 {
        return None;
    }
    let prev = bytes[name_pos - 1];
    if prev == b'<' {
        return Some(name_pos - 1);
    }
    if prev == b':' {
        let i = name_pos - 1;
        let mut j = i;
        while j > 0 && {
            let c = bytes[j - 1];
            c.is_ascii_alphanumeric() || c == b'_'
        } {
            j -= 1;
        }
        if j < i && j > 0 && bytes[j - 1] == b'<' {
            return Some(j - 1);
        }
    }
    None
}

/// 查找未被引用字符包裹的 invoke 开标签，返回指向 `<` 的字节位置。
fn find_invoke_start(buffer: &str) -> Option<usize> {
    let mut search = 0;
    while let Some(rel) = buffer[search..].find("invoke") {
        let name_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(buffer, name_pos) {
            let after = name_pos + "invoke".len();
            let next_ok = buffer.as_bytes().get(after).map_or(true, |c| {
                c.is_ascii_whitespace() || *c == b'>' || *c == b'/'
            });
            let has_quote_before = lt > 0 && is_quote_char(buffer, lt - 1);
            if next_ok && !has_quote_before {
                return Some(lt);
            }
        }
        search = name_pos + "invoke".len();
    }
    None
}

fn find_invoke_block_end(buffer: &str, start: usize) -> Option<usize> {
    let boundary = find_next_invoke_open(buffer, start).unwrap_or(buffer.len());
    find_last_invoke_close(buffer, start, boundary)
}

fn find_next_invoke_open(buffer: &str, start: usize) -> Option<usize> {
    let after_open = match buffer[start..].find('>') {
        Some(rel) => start + rel + 1,
        None => return None,
    };
    let region = &buffer[after_open..];
    let mut search = 0usize;
    while let Some(rel) = region[search..].find("invoke") {
        let name_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(region, name_pos) {
            let after = name_pos + "invoke".len();
            let next_ok = region.as_bytes().get(after).map_or(true, |c| {
                c.is_ascii_whitespace() || *c == b'>' || *c == b'/'
            });
            if next_ok {
                return Some(after_open + lt);
            }
        }
        search = name_pos + "invoke".len();
    }
    None
}

fn find_last_invoke_close(buffer: &str, from: usize, boundary: usize) -> Option<usize> {
    let region_end = boundary.min(buffer.len());
    if from >= region_end {
        return None;
    }
    let region = &buffer[from..region_end];
    let bytes = region.as_bytes();
    let mut search = 0usize;
    let mut last: Option<usize> = None;
    while let Some(rel) = region[search..].find("invoke>") {
        let name_pos = search + rel;
        if name_pos >= 2 && &region[name_pos - 2..name_pos] == "</" {
            last = Some(from + name_pos + "invoke>".len());
        } else if name_pos >= 1 && bytes[name_pos - 1] == b':' {
            let mut j = name_pos - 1;
            while j > 0 && {
                let c = bytes[j - 1];
                c.is_ascii_alphanumeric() || c == b'_'
            } {
                j -= 1;
            }
            if j >= 2 && &region[j - 2..j] == "</" {
                last = Some(from + name_pos + "invoke>".len());
            }
        }
        search = name_pos + "invoke>".len();
    }
    last
}

fn extract_name_attr(tag: &str) -> Option<String> {
    let needle = "name=\"";
    let rel = tag.find(needle)?;
    let start = rel + needle.len();
    let end_rel = tag[start..].find('"')?;
    Some(tag[start..start + end_rel].to_string())
}

fn restore_xml_parameter_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed == "true" {
        serde_json::Value::Bool(true)
    } else if trimmed == "false" {
        serde_json::Value::Bool(false)
    } else if trimmed == "null" {
        serde_json::Value::Null
    } else if !trimmed.is_empty()
        && trimmed
            .strip_prefix('-')
            .unwrap_or(trimmed)
            .chars()
            .all(|ch| ch.is_ascii_digit())
    {
        trimmed
            .parse::<i64>()
            .map(|n| serde_json::Value::Number(n.into()))
            .unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
    } else if looks_like_float(trimmed) {
        trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(raw.to_string()))
    } else {
        serde_json::Value::String(raw.to_string())
    }
}

fn looks_like_float(value: &str) -> bool {
    if !value.contains('.') {
        return false;
    }
    let value = value.strip_prefix('-').unwrap_or(value);
    let mut parts = value.split('.');
    let before = parts.next().unwrap_or_default();
    let after = parts.next().unwrap_or_default();
    parts.next().is_none()
        && (!before.is_empty() || !after.is_empty())
        && before.chars().all(|ch| ch.is_ascii_digit())
        && after.chars().all(|ch| ch.is_ascii_digit())
}

/// 解析一个完整 invoke 块，抠出 (tool_name, input_json_string)。
fn parse_invoke_block(block: &str) -> Option<(String, String)> {
    let open_end = block.find('>')?;
    let open_tag = &block[..=open_end];
    let tool_name = extract_name_attr(open_tag)?;
    if tool_name.is_empty() {
        return None;
    }

    let mut map = serde_json::Map::new();
    let body = &block[open_end + 1..];
    let mut cursor = 0usize;
    while let Some(rel) = body[cursor..].find("parameter name=\"") {
        let name_kw = cursor + rel;
        if open_tag_lt_pos(body, name_kw).is_none() {
            cursor = name_kw + "parameter".len();
            continue;
        }
        let tag_gt = match body[name_kw..].find('>') {
            Some(r) => name_kw + r,
            None => break,
        };
        let param_open_tag = &body[name_kw..tag_gt + 1];
        let key = match extract_name_attr(param_open_tag) {
            Some(k) => k,
            None => {
                cursor = tag_gt + 1;
                continue;
            }
        };
        let val_start = tag_gt + 1;
        let (close_start, close_end) = match find_param_close(body, val_start) {
            Some(pair) => pair,
            None => break,
        };
        let value = &body[val_start..close_start];
        map.insert(key, restore_xml_parameter_value(value));
        cursor = close_end;
    }

    serde_json::to_string(&serde_json::Value::Object(map))
        .ok()
        .map(|input| (tool_name, input))
}

fn find_param_close(body: &str, from: usize) -> Option<(usize, usize)> {
    let boundary = find_next_param_open(body, from).unwrap_or(body.len());
    let region = &body[from..boundary];
    let kw = "parameter>";
    let mut last: Option<(usize, usize)> = None;
    let mut search = 0usize;
    let bytes = region.as_bytes();
    while let Some(rel) = region[search..].find(kw) {
        let name_pos = search + rel;
        if name_pos >= 2 && &region[name_pos - 2..name_pos] == "</" {
            last = Some((from + name_pos - 2, from + name_pos + kw.len()));
        } else if name_pos >= 1 && bytes[name_pos - 1] == b':' {
            let mut j = name_pos - 1;
            while j > 0 && {
                let c = bytes[j - 1];
                c.is_ascii_alphanumeric() || c == b'_'
            } {
                j -= 1;
            }
            if j >= 2 && &region[j - 2..j] == "</" {
                last = Some((from + j - 2, from + name_pos + kw.len()));
            }
        }
        search = name_pos + kw.len();
    }
    last
}

fn find_next_param_open(body: &str, from: usize) -> Option<usize> {
    let mut search = from;
    while let Some(rel) = body[search..].find("parameter name=\"") {
        let kw_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(body, kw_pos) {
            return Some(lt);
        }
        search = kw_pos + "parameter".len();
    }
    None
}

const STRAY_INVOKE_TOKENS: &[&str] = &["call", "count", "card"];
const REPEAT_GUARD_TRIP_THRESHOLD: u32 = 32;

fn collapse_stray_token_floods(text: &str) -> std::borrow::Cow<'_, str> {
    let mut last_line = "";
    let mut run: u32 = 0;
    let mut cut_at: Option<usize> = None;
    let mut offset = 0usize;
    for segment in text.split_inclusive('\n') {
        let line = segment.trim();
        if STRAY_INVOKE_TOKENS.contains(&line) {
            if line == last_line {
                run += 1;
            } else {
                last_line = line;
                run = 1;
            }
            if run >= REPEAT_GUARD_TRIP_THRESHOLD {
                cut_at = Some(offset);
                break;
            }
        } else if !line.is_empty() {
            last_line = line;
            run = 0;
        }
        offset += segment.len();
    }
    match cut_at {
        Some(pos) => std::borrow::Cow::Owned(text[..pos].to_string()),
        None => std::borrow::Cow::Borrowed(text),
    }
}

fn strip_trailing_stray_tokens(before: &str) -> &str {
    let mut end = before.len();
    loop {
        let bytes = before.as_bytes();
        let mut e = end;
        while e > 0 && (bytes[e - 1] == b'\n' || bytes[e - 1] == b'\r') {
            e -= 1;
        }
        let line_start = before[..e].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let last_line = before[line_start..e].trim();
        if STRAY_INVOKE_TOKENS.contains(&last_line) {
            end = line_start;
            if end == 0 {
                return "";
            }
        } else {
            break;
        }
    }
    &before[..end]
}

fn strip_trailing_function_calls_open(before: &str) -> Option<&str> {
    let trimmed = before.trim_end_matches([' ', '\t', '\r', '\n']);
    if !trimmed.ends_with('>') {
        return None;
    }
    let lt = trimmed.rfind('<')?;
    let tag = &trimmed[lt..];
    if is_function_calls_open_tag(tag) {
        Some(&before[..lt])
    } else {
        None
    }
}

fn strip_trailing_tool_prefixes(before: &str) -> &str {
    let mut current = before;
    loop {
        let stripped = strip_trailing_stray_tokens(current);
        if stripped.len() != current.len() {
            current = stripped;
            continue;
        }
        if let Some(stripped) = strip_trailing_function_calls_open(current) {
            current = stripped;
            continue;
        }
        return current;
    }
}

fn is_function_calls_open_tag(tag: &str) -> bool {
    let Some(inner) = tag.strip_prefix('<').and_then(|s| s.strip_suffix('>')) else {
        return false;
    };
    if inner.starts_with('/') {
        return false;
    }
    let name = inner.split_whitespace().next().unwrap_or_default();
    name == "function_calls" || name.ends_with(":function_calls")
}

fn leading_function_calls_close_len(rest: &str) -> usize {
    let leading_ws = rest.len() - rest.trim_start().len();
    let s = &rest[leading_ws..];
    let Some(end) = s.find('>') else {
        return 0;
    };
    let tag = &s[..=end];
    let Some(inner) = tag.strip_prefix("</").and_then(|v| v.strip_suffix('>')) else {
        return 0;
    };
    if inner == "function_calls" || inner.ends_with(":function_calls") {
        let mut total = leading_ws + end + 1;
        while rest
            .as_bytes()
            .get(total)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            total += 1;
        }
        total
    } else {
        0
    }
}

fn strip_leading_function_calls_close(rest: &str) -> &str {
    let len = leading_function_calls_close_len(rest);
    &rest[len..]
}

fn invoke_looks_like_real_leak(before: &str) -> bool {
    let trimmed = before.trim_end_matches([' ', '\t']);
    trimmed.is_empty() || trimmed.ends_with('\n') || trimmed.ends_with('\r')
}

fn advance_code_fence_state(open: &mut bool, partial: &mut String, text: &str) {
    for ch in text.chars() {
        if ch == '\n' {
            if partial.trim_start().starts_with("```") {
                *open = !*open;
            }
            partial.clear();
        } else {
            partial.push(ch);
        }
    }
}

fn fence_open_after(open: bool, partial: &str, text: &str) -> bool {
    let mut o = open;
    let mut p = partial.to_string();
    advance_code_fence_state(&mut o, &mut p, text);
    o
}

fn partial_invoke_tag_suffix_len(buf: &str) -> usize {
    if let Some(lt) = buf.rfind('<') {
        if !buf[lt..].contains('>') {
            return buf.len() - lt;
        }
    }
    0
}

fn trailing_tool_protocol_tail_len(buf: &str) -> usize {
    let mut keep = partial_invoke_tag_suffix_len(buf);

    let trimmed_end = buf.trim_end_matches([' ', '\t', '\r', '\n']);
    let trailing_ws = buf.len() - trimmed_end.len();
    if let Some(line_start) = trimmed_end.rfind('\n').map(|p| p + 1).or(Some(0)) {
        let line = trimmed_end[line_start..].trim();
        if STRAY_INVOKE_TOKENS.contains(&line) {
            keep = keep.max(buf.len() - line_start);
        }
    }

    if let Some(pos) = find_trailing_function_calls_open(buf) {
        keep = keep.max(buf.len() - pos);
    }

    keep.max(trailing_ws.min(buf.len()))
}

fn find_trailing_function_calls_open(buf: &str) -> Option<usize> {
    let mut search = 0usize;
    let mut last = None;
    while let Some(rel) = buf[search..].find("function_calls") {
        let name_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(buf, name_pos) {
            let after = name_pos + "function_calls".len();
            let next_ok = buf.as_bytes().get(after).map_or(true, |c| {
                c.is_ascii_whitespace() || *c == b'>' || *c == b'/'
            });
            if next_ok {
                last = Some(lt);
            }
        }
        search = name_pos + "function_calls".len();
    }
    let lt = last?;
    let tail = &buf[lt..];
    if tail.contains("</function_calls>") || tail.contains("</antml:function_calls>") {
        return None;
    }
    if tail.contains("<invoke") {
        return None;
    }
    Some(lt)
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                if let Some(value) = map.get(key) {
                    out.insert(key.clone(), canonical_json(value));
                }
            }
            serde_json::Value::Object(out)
        }
        _ => value.clone(),
    }
}

pub(crate) fn tool_use_signature(name: &str, input: &serde_json::Value) -> String {
    format!(
        "{}|{}",
        name,
        serde_json::to_string(&canonical_json(input)).unwrap_or_else(|_| "null".to_string())
    )
}

fn tool_use_signature_from_json_str(name: &str, input_json: &str) -> String {
    let input = if input_json.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(input_json).unwrap_or_else(|_| serde_json::json!({}))
    };
    tool_use_signature(name, &input)
}

/// 一次性把完整 assistant 文本里的字面 `<invoke>` 恢复成 Anthropic content blocks。
pub(crate) fn extract_invoke_content_blocks(
    text: &str,
    known_tool_names: &HashSet<String>,
    tool_name_map: &HashMap<String, String>,
) -> Vec<serde_json::Value> {
    let collapsed = collapse_stray_token_floods(text);
    let text: &str = &collapsed;
    let mut blocks = Vec::new();
    let mut pending_text = String::new();
    let mut fence_open = false;
    let mut fence_partial = String::new();

    fn push_text(blocks: &mut Vec<serde_json::Value>, pending: &mut String) {
        if !pending.is_empty() {
            blocks.push(json!({"type": "text", "text": std::mem::take(pending)}));
        }
    }

    let mut rest = text;
    loop {
        let Some(start) = find_invoke_start(rest) else {
            pending_text.push_str(rest);
            break;
        };
        let Some(end) = find_invoke_block_end(rest, start) else {
            pending_text.push_str(rest);
            break;
        };

        let before = &rest[..start];
        let stripped_before = strip_trailing_tool_prefixes(before);
        let fence_after_before = fence_open_after(fence_open, &fence_partial, stripped_before);
        let parsed = parse_invoke_block(&rest[start..end]);
        let name_known = parsed
            .as_ref()
            .map(|(name, _)| known_tool_names.contains(name))
            .unwrap_or(false);

        if invoke_looks_like_real_leak(stripped_before) && !fence_after_before && name_known {
            if !stripped_before.is_empty() {
                advance_code_fence_state(&mut fence_open, &mut fence_partial, stripped_before);
                pending_text.push_str(stripped_before);
            }
            push_text(&mut blocks, &mut pending_text);
            let (name, input_json) = parsed.expect("parsed is Some when name_known");
            let input: serde_json::Value =
                serde_json::from_str(&input_json).unwrap_or_else(|_| json!({}));
            let name = tool_name_map.get(&name).cloned().unwrap_or(name);
            let tool_use_id = format!("toolu_{}", Uuid::new_v4().to_string().replace('-', ""));
            blocks.push(json!({
                "type": "tool_use",
                "id": tool_use_id,
                "name": name,
                "input": input,
            }));
        } else {
            let chunk = &rest[..end];
            advance_code_fence_state(&mut fence_open, &mut fence_partial, chunk);
            pending_text.push_str(chunk);
        }
        rest = strip_leading_function_calls_close(&rest[end..]);
    }

    push_text(&mut blocks, &mut pending_text);
    blocks
}

/// 从完整文本中提取 thinking 块（用于非流式响应）
///
/// 使用与流式处理相同的标签检测逻辑（引用字符过滤），确保一致性。
/// 非流式场景下文本已完整，无需处理跨 chunk 分割问题。
///
/// # 返回值
/// - `(Some(thinking_content), remaining_text)` — 检测到有效 thinking 块
/// - `(None, original_text)` — 未检测到，原样返回
pub(crate) fn extract_thinking_from_complete_text(text: &str) -> (Option<String>, String) {
    let start_pos = match find_real_thinking_start_tag(text) {
        Some(pos) => pos,
        None => return (None, text.to_string()),
    };

    let before = &text[..start_pos];
    let after_open = &text[start_pos + "<thinking>".len()..];

    // 查找结束标签：优先匹配带 \n\n 后缀的，退而使用末尾匹配
    let (thinking_raw, text_after) = if let Some(end_pos) = find_real_thinking_end_tag(after_open) {
        (
            &after_open[..end_pos],
            &after_open[end_pos + "</thinking>\n\n".len()..],
        )
    } else if let Some(end_pos) = find_real_thinking_end_tag_at_buffer_end(after_open) {
        let after_tag = end_pos + "</thinking>".len();
        (&after_open[..end_pos], after_open[after_tag..].trim_start())
    } else {
        // 找不到有效的结束标签，不做提取
        return (None, text.to_string());
    };

    // 剥离开头的换行符（与流式处理一致：模型输出 <thinking>\n）
    let thinking_content = thinking_raw.strip_prefix('\n').unwrap_or(thinking_raw);

    // 组装剩余文本：跳过纯空白的 before 部分
    let mut remaining = String::new();
    if !before.trim().is_empty() {
        remaining.push_str(before);
    }
    remaining.push_str(text_after);

    if thinking_content.is_empty() {
        (None, remaining)
    } else {
        (Some(thinking_content.to_string()), remaining)
    }
}

/// SSE 事件
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: String,
    pub data: serde_json::Value,
}

impl SseEvent {
    pub fn new(event: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            data,
        }
    }

    /// 格式化为 SSE 字符串
    pub fn to_sse_string(&self) -> String {
        format!(
            "event: {}\ndata: {}\n\n",
            self.event,
            serde_json::to_string(&self.data).unwrap_or_default()
        )
    }
}

/// 内容块状态
#[derive(Debug, Clone)]
struct BlockState {
    block_type: String,
    started: bool,
    stopped: bool,
}

impl BlockState {
    fn new(block_type: impl Into<String>) -> Self {
        Self {
            block_type: block_type.into(),
            started: false,
            stopped: false,
        }
    }
}

/// SSE 状态管理器
///
/// 确保 SSE 事件序列符合 Claude API 规范：
/// 1. message_start 只能出现一次
/// 2. content_block 必须先 start 再 delta 再 stop
/// 3. message_delta 只能出现一次，且在所有 content_block_stop 之后
/// 4. message_stop 在最后
#[derive(Debug)]
pub struct SseStateManager {
    /// message_start 是否已发送
    message_started: bool,
    /// message_delta 是否已发送
    message_delta_sent: bool,
    /// 活跃的内容块状态
    active_blocks: HashMap<i32, BlockState>,
    /// 消息是否已结束
    message_ended: bool,
    /// 下一个块索引
    next_block_index: i32,
    /// 当前 stop_reason
    stop_reason: Option<String>,
    /// 是否有工具调用
    has_tool_use: bool,
}

impl Default for SseStateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SseStateManager {
    pub fn new() -> Self {
        Self {
            message_started: false,
            message_delta_sent: false,
            active_blocks: HashMap::new(),
            message_ended: false,
            next_block_index: 0,
            stop_reason: None,
            has_tool_use: false,
        }
    }

    /// 判断指定块是否处于可接收 delta 的打开状态
    fn is_block_open_of_type(&self, index: i32, expected_type: &str) -> bool {
        self.active_blocks
            .get(&index)
            .is_some_and(|b| b.started && !b.stopped && b.block_type == expected_type)
    }

    /// 获取下一个块索引
    pub fn next_block_index(&mut self) -> i32 {
        let index = self.next_block_index;
        self.next_block_index += 1;
        index
    }

    /// 记录工具调用
    pub fn set_has_tool_use(&mut self, has: bool) {
        self.has_tool_use = has;
    }

    /// 设置 stop_reason
    pub fn set_stop_reason(&mut self, reason: impl Into<String>) {
        self.stop_reason = Some(reason.into());
    }

    /// 检查是否存在非 thinking 类型的内容块（如 text 或 tool_use）
    fn has_non_thinking_blocks(&self) -> bool {
        self.active_blocks
            .values()
            .any(|b| b.block_type != "thinking")
    }

    /// 关闭所有未关闭的内容块。
    fn close_open_blocks(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        for (index, block) in self.active_blocks.iter_mut() {
            if block.started && !block.stopped {
                events.push(SseEvent::new(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                ));
                block.stopped = true;
            }
        }

        events
    }

    /// 获取最终的 stop_reason
    pub fn get_stop_reason(&self) -> String {
        if let Some(ref reason) = self.stop_reason {
            reason.clone()
        } else if self.has_tool_use {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        }
    }

    /// 处理 message_start 事件
    pub fn handle_message_start(&mut self, event: serde_json::Value) -> Option<SseEvent> {
        if self.message_started {
            tracing::debug!("跳过重复的 message_start 事件");
            return None;
        }
        self.message_started = true;
        Some(SseEvent::new("message_start", event))
    }

    /// 处理 content_block_start 事件
    pub fn handle_content_block_start(
        &mut self,
        index: i32,
        block_type: &str,
        data: serde_json::Value,
    ) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 如果是 tool_use 块，先关闭之前的文本块
        if block_type == "tool_use" {
            self.has_tool_use = true;
            for (block_index, block) in self.active_blocks.iter_mut() {
                if block.block_type == "text" && block.started && !block.stopped {
                    // 自动发送 content_block_stop 关闭文本块
                    events.push(SseEvent::new(
                        "content_block_stop",
                        json!({
                            "type": "content_block_stop",
                            "index": block_index
                        }),
                    ));
                    block.stopped = true;
                }
            }
        }

        // 检查块是否已存在
        if let Some(block) = self.active_blocks.get_mut(&index) {
            if block.started {
                tracing::debug!("块 {} 已启动，跳过重复的 content_block_start", index);
                return events;
            }
            block.started = true;
        } else {
            let mut block = BlockState::new(block_type);
            block.started = true;
            self.active_blocks.insert(index, block);
        }

        events.push(SseEvent::new("content_block_start", data));
        events
    }

    /// 处理 content_block_delta 事件
    pub fn handle_content_block_delta(
        &mut self,
        index: i32,
        data: serde_json::Value,
    ) -> Option<SseEvent> {
        // 确保块已启动
        if let Some(block) = self.active_blocks.get(&index) {
            if !block.started || block.stopped {
                tracing::warn!(
                    "块 {} 状态异常: started={}, stopped={}",
                    index,
                    block.started,
                    block.stopped
                );
                return None;
            }
        } else {
            // 块不存在，可能需要先创建
            tracing::warn!("收到未知块 {} 的 delta 事件", index);
            return None;
        }

        Some(SseEvent::new("content_block_delta", data))
    }

    /// 处理 content_block_stop 事件
    pub fn handle_content_block_stop(&mut self, index: i32) -> Option<SseEvent> {
        if let Some(block) = self.active_blocks.get_mut(&index) {
            if block.stopped {
                tracing::debug!("块 {} 已停止，跳过重复的 content_block_stop", index);
                return None;
            }
            block.stopped = true;
            return Some(SseEvent::new(
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            ));
        }
        None
    }

    /// 生成最终事件序列
    #[allow(dead_code)]
    pub fn generate_final_events(
        &mut self,
        input_tokens: i32,
        output_tokens: i32,
    ) -> Vec<SseEvent> {
        self.generate_final_events_with_usage(json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        }))
    }

    /// 生成带完整 usage 对象的最终事件序列
    pub fn generate_final_events_with_usage(&mut self, usage: serde_json::Value) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 关闭所有未关闭的块
        events.extend(self.close_open_blocks());

        // 发送 message_delta
        if !self.message_delta_sent {
            self.message_delta_sent = true;
            events.push(SseEvent::new(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": self.get_stop_reason(),
                        "stop_sequence": null
                    },
                    "usage": usage
                }),
            ));
        }

        // 发送 message_stop
        if !self.message_ended {
            self.message_ended = true;
            events.push(SseEvent::new(
                "message_stop",
                json!({ "type": "message_stop" }),
            ));
        }

        events
    }
}

const DEFAULT_CONTEXT_WINDOW_TOKENS: i32 = 200_000;

/// 流处理上下文
pub struct StreamContext {
    /// SSE 状态管理器
    pub state_manager: SseStateManager,
    /// 下游请求的模型名称，用于响应保持 Anthropic 兼容。
    pub model: String,
    /// 当前上游模型目录中的输入窗口大小，用于 contextUsageEvent 百分比换算。
    pub context_window_tokens: i32,
    /// 消息 ID
    pub message_id: String,
    /// 输入 tokens（估算值）
    pub input_tokens: i32,
    /// 从 contextUsageEvent 计算的实际输入 tokens
    pub context_input_tokens: Option<i32>,
    /// metadataEvent 提供的准确 token usage
    pub metadata_usage: Option<MetadataTokenUsage>,
    /// 输出 tokens 累计
    pub output_tokens: i32,
    /// 工具块索引映射 (tool_id -> block_index)
    pub tool_block_indices: HashMap<String, i32>,
    /// 工具输入 JSON 片段累计，用于 stop 时生成稳定去重签名。
    tool_input_buffers: HashMap<String, String>,
    /// 从文本泄漏中恢复出的工具调用，延迟到流末尾发出，以便和后续结构化 toolUseEvent 去重。
    pending_leaked_tools: Vec<(String, String, String)>,
    /// 工具名称反向映射（短名称 → 原始名称），用于响应时还原
    pub tool_name_map: HashMap<String, String>,
    /// 本次请求声明的工具名集合。字面 `<invoke>` 只有命中该集合才会恢复成 tool_use。
    pub known_tool_names: HashSet<String>,
    /// 已发出的工具调用签名，用于结构化 toolUseEvent 与文本泄漏恢复之间去重。
    seen_tool_sigs: HashSet<String>,
    /// 代码围栏状态；围栏内的 `<invoke>` 视为正文展示，不恢复。
    code_fence_open: bool,
    /// 围栏检测的未完成行缓冲。
    fence_scan_partial: String,
    /// thinking 是否启用
    pub thinking_enabled: bool,
    /// 是否把 XML `<thinking>` 文本提取为 unsigned thinking block
    pub extract_xml_thinking: bool,
    /// thinking 内容缓冲区
    pub thinking_buffer: String,
    /// 字面 `<invoke>` 跨 chunk 嗅探缓冲区。
    invoke_sniff_buffer: String,
    /// 是否在 thinking 块内
    pub in_thinking_block: bool,
    /// thinking 块是否已提取完成
    pub thinking_extracted: bool,
    /// thinking 块索引
    pub thinking_block_index: Option<i32>,
    /// 文本块索引（thinking 启用时动态分配）
    pub text_block_index: Option<i32>,
    /// 是否需要剥离 thinking 内容开头的换行符
    /// 模型输出 `<thinking>\n` 时，`\n` 可能与标签在同一 chunk 或下一 chunk
    strip_thinking_leading_newline: bool,
    /// 是否已收到原生 reasoningContentEvent
    native_reasoning_seen: bool,
    /// 原生 reasoning 累计内容，用于从快照计算 delta
    native_reasoning_content: String,
    /// 原生 reasoning 签名，按 Anthropic SSE 规范在 thinking block stop 前发送 signature_delta。
    native_reasoning_signature: Option<String>,
    /// 原生 reasoning 签名是否已通过 signature_delta 发送。
    native_reasoning_signature_sent: bool,
    /// 上游流内错误，最终以 SSE error 事件暴露
    stream_error: Option<(String, String)>,
    /// 无 metadata 时使用的本地 prompt-cache usage 模拟结果
    pub simulated_usage: Option<super::cache::CacheSimulation>,
    /// 本地 prompt-cache usage 模拟模式。
    pub simulation_mode: PromptCacheSimulationMode,
    /// 仅用于下游上报的 cache usage 改写策略。
    reported_cache_usage_policy: Option<super::cache::ReportedCacheUsagePolicy>,
    /// 最近一次最终 usage，用于请求级记录。
    final_usage: Option<super::cache::CacheUsage>,
    /// 最近一次最终下游上报 usage，用于请求级记录。
    final_reported_usage: Option<super::cache::CacheUsage>,
    /// stray token 复读熔断：最近一行。
    repeat_guard_last_line: String,
    /// stray token 复读熔断：连续次数。
    repeat_guard_run: u32,
    /// stray token 复读熔断：触发后本轮剩余文本丢弃。
    repeat_guard_tripped: bool,
}

impl StreamContext {
    /// 创建启用thinking的StreamContext
    #[allow(dead_code)]
    pub fn new_with_thinking(
        model: impl Into<String>,
        input_tokens: i32,
        thinking_enabled: bool,
        tool_name_map: HashMap<String, String>,
    ) -> Self {
        Self::new_with_thinking_with_known_tools(
            model,
            input_tokens,
            thinking_enabled,
            tool_name_map,
            HashSet::new(),
        )
    }

    pub fn new_with_thinking_with_known_tools(
        model: impl Into<String>,
        input_tokens: i32,
        thinking_enabled: bool,
        tool_name_map: HashMap<String, String>,
        known_tool_names: HashSet<String>,
    ) -> Self {
        Self::new_with_simulation_with_known_tools(
            model,
            input_tokens,
            DEFAULT_CONTEXT_WINDOW_TOKENS,
            thinking_enabled,
            true,
            tool_name_map,
            known_tool_names,
            None,
            PromptCacheSimulationMode::Disabled,
        )
    }

    #[allow(dead_code)]
    pub fn new_with_simulation(
        model: impl Into<String>,
        input_tokens: i32,
        context_window_tokens: i32,
        thinking_enabled: bool,
        extract_xml_thinking: bool,
        tool_name_map: HashMap<String, String>,
        simulated_usage: Option<super::cache::CacheSimulation>,
        simulation_mode: PromptCacheSimulationMode,
    ) -> Self {
        Self::new_with_simulation_with_known_tools(
            model,
            input_tokens,
            context_window_tokens,
            thinking_enabled,
            extract_xml_thinking,
            tool_name_map,
            HashSet::new(),
            simulated_usage,
            simulation_mode,
        )
    }

    pub fn new_with_simulation_with_known_tools(
        model: impl Into<String>,
        input_tokens: i32,
        context_window_tokens: i32,
        thinking_enabled: bool,
        extract_xml_thinking: bool,
        tool_name_map: HashMap<String, String>,
        known_tool_names: HashSet<String>,
        simulated_usage: Option<super::cache::CacheSimulation>,
        simulation_mode: PromptCacheSimulationMode,
    ) -> Self {
        Self {
            state_manager: SseStateManager::new(),
            model: model.into(),
            context_window_tokens,
            message_id: envelope::message_id(),
            input_tokens,
            context_input_tokens: None,
            metadata_usage: None,
            output_tokens: 0,
            tool_block_indices: HashMap::new(),
            tool_input_buffers: HashMap::new(),
            pending_leaked_tools: Vec::new(),
            tool_name_map,
            known_tool_names,
            seen_tool_sigs: HashSet::new(),
            code_fence_open: false,
            fence_scan_partial: String::new(),
            thinking_enabled,
            extract_xml_thinking,
            thinking_buffer: String::new(),
            invoke_sniff_buffer: String::new(),
            in_thinking_block: false,
            thinking_extracted: false,
            thinking_block_index: None,
            text_block_index: None,
            strip_thinking_leading_newline: false,
            native_reasoning_seen: false,
            native_reasoning_content: String::new(),
            native_reasoning_signature: None,
            native_reasoning_signature_sent: false,
            stream_error: None,
            simulated_usage,
            simulation_mode,
            reported_cache_usage_policy: None,
            final_usage: None,
            final_reported_usage: None,
            repeat_guard_last_line: String::new(),
            repeat_guard_run: 0,
            repeat_guard_tripped: false,
        }
    }

    pub fn set_reported_cache_usage_policy(
        &mut self,
        policy: Option<super::cache::ReportedCacheUsagePolicy>,
    ) {
        self.reported_cache_usage_policy = policy;
    }

    fn reported_usage_for_downstream(
        &self,
        usage: super::cache::CacheUsage,
    ) -> super::cache::CacheUsage {
        if self.simulation_mode != PromptCacheSimulationMode::HighCache
            || self.simulated_usage.is_none()
            || self
                .metadata_usage
                .as_ref()
                .is_some_and(|usage| !super::cache::metadata_cache_is_empty(usage))
        {
            return usage;
        }

        self.reported_cache_usage_policy
            .clone()
            .map(|policy| {
                usage.with_reported_cache_usage_policy_and_raw(
                    policy,
                    super::cache::RawUsage::uncached(self.input_tokens, usage.output_tokens),
                )
            })
            .unwrap_or(usage)
    }

    /// 生成 message_start 事件
    pub fn create_message_start_event_with_reported_usage_mapper<F>(
        &self,
        usage_mapper: F,
    ) -> serde_json::Value
    where
        F: FnOnce(super::cache::CacheUsage) -> super::cache::CacheUsage,
    {
        // message_start is emitted before final context/metadata usage is known.
        // Keep input/cache neutral here; the final message_delta carries the
        // authoritative downstream usage. Some callers only overwrite positive
        // fields, so early non-zero cache values can become stale.
        let usage = usage_mapper(super::cache::CacheUsage {
            total_input_tokens: self.input_tokens,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        })
        .to_anthropic_usage_json();
        json!({
            "type": "message_start",
            "message": {
                "id": self.message_id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": self.model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage
            }
        })
    }

    /// 生成初始事件序列 (message_start + 文本块 start)
    ///
    /// 当 thinking 启用时，不在初始化时创建文本块，而是等到实际收到内容时再创建。
    /// 这样可以确保 thinking 块（索引 0）在文本块（索引 1）之前。
    #[cfg(test)]
    pub fn generate_initial_events(&mut self) -> Vec<SseEvent> {
        self.generate_initial_events_with_reported_usage_mapper(|reported_usage| reported_usage)
    }

    pub fn generate_initial_events_with_reported_usage_mapper<F>(
        &mut self,
        usage_mapper: F,
    ) -> Vec<SseEvent>
    where
        F: FnOnce(super::cache::CacheUsage) -> super::cache::CacheUsage,
    {
        let mut events = Vec::new();

        // message_start
        let msg_start = self.create_message_start_event_with_reported_usage_mapper(usage_mapper);
        if let Some(event) = self.state_manager.handle_message_start(msg_start) {
            events.push(event);
        }

        // 如果启用了 thinking，不在这里创建文本块
        // thinking 块和文本块会在 process_content_with_thinking 中按正确顺序创建
        if self.thinking_enabled {
            return events;
        }

        // 创建初始文本块（仅在未启用 thinking 时）
        let text_block_index = self.state_manager.next_block_index();
        self.text_block_index = Some(text_block_index);
        let text_block_events = self.state_manager.handle_content_block_start(
            text_block_index,
            "text",
            json!({
                "type": "content_block_start",
                "index": text_block_index,
                "content_block": {
                    "type": "text",
                    "text": ""
                }
            }),
        );
        events.extend(text_block_events);

        events
    }

    /// 处理 Kiro 事件并转换为 Anthropic SSE 事件
    pub fn process_kiro_event(&mut self, event: &Event) -> Vec<SseEvent> {
        match event {
            Event::AssistantResponse(resp) => self.process_assistant_response(&resp.content),
            Event::Code(code) => self.process_assistant_response(&code.content),
            Event::ReasoningContent(reasoning) => self.process_reasoning_content(reasoning),
            Event::ToolUse(tool_use) => self.process_tool_use(tool_use),
            Event::Metadata(metadata) => {
                if let Some(token_usage) = &metadata.token_usage {
                    self.output_tokens = token_usage.output_tokens;
                    self.metadata_usage = Some(token_usage.clone());
                    tracing::debug!(
                        input_tokens = token_usage.input_tokens(),
                        output_tokens = token_usage.output_tokens,
                        cache_read_input_tokens = token_usage.cache_read_input_tokens,
                        cache_write_input_tokens = token_usage.cache_write_input_tokens,
                        "收到 metadataEvent token usage"
                    );
                }
                Vec::new()
            }
            Event::ContextUsage(context_usage) => {
                // 从上下文使用百分比计算实际的 input_tokens
                let window_size = self.context_window_tokens;
                let actual_input_tokens =
                    (context_usage.context_usage_percentage * (window_size as f64) / 100.0) as i32;
                self.context_input_tokens = Some(actual_input_tokens);
                // 上下文使用量达到 100% 时，设置 stop_reason 为 model_context_window_exceeded
                if context_usage.context_usage_percentage >= 100.0 {
                    self.state_manager
                        .set_stop_reason("model_context_window_exceeded");
                }
                tracing::debug!(
                    "收到 contextUsageEvent: {}%, 计算 input_tokens: {}",
                    context_usage.context_usage_percentage,
                    actual_input_tokens
                );
                Vec::new()
            }
            Event::MessageMetadata(metadata) => {
                if let Some(token_usage) = &metadata.token_usage {
                    self.output_tokens = token_usage.output_tokens;
                    self.metadata_usage = Some(token_usage.clone());
                    tracing::debug!(
                        conversation_id = ?metadata.conversation_id,
                        utterance_id = ?metadata.utterance_id,
                        input_tokens = token_usage.input_tokens(),
                        output_tokens = token_usage.output_tokens,
                        cache_read_input_tokens = token_usage.cache_read_input_tokens,
                        cache_write_input_tokens = token_usage.cache_write_input_tokens,
                        "收到 messageMetadataEvent token usage"
                    );
                }
                tracing::debug!(
                    conversation_id = ?metadata.conversation_id,
                    utterance_id = ?metadata.utterance_id,
                    "收到 messageMetadataEvent"
                );
                Vec::new()
            }
            Event::Metering(metering) => {
                tracing::debug!(usage = metering.usage, "收到 meteringEvent");
                Vec::new()
            }
            Event::InvalidState(invalid) => {
                let message = invalid.error_text();
                tracing::warn!(
                    reason = %invalid.reason,
                    message = %message,
                    "收到 invalidStateEvent"
                );
                self.record_stream_error("invalid_request_error", message);
                Vec::new()
            }
            Event::Error {
                error_code,
                error_message,
            } => {
                tracing::error!("收到错误事件: {} - {}", error_code, error_message);
                self.record_stream_error("api_error", error_message.clone());
                Vec::new()
            }
            Event::Exception {
                exception_type,
                message,
            } => {
                // 处理 ContentLengthExceededException
                if exception_type == "ContentLengthExceededException" {
                    self.state_manager.set_stop_reason("max_tokens");
                }
                tracing::warn!("收到异常事件: {} - {}", exception_type, message);
                self.record_stream_error("api_error", message.clone());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// 记录上游流错误，最终转换为 Anthropic SSE error 事件。
    pub fn record_stream_error(
        &mut self,
        error_type: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.stream_error = Some((error_type.into(), message.into()));
    }

    /// 当前流是否已经记录了上游或协议错误。
    pub fn has_stream_error(&self) -> bool {
        self.stream_error.is_some()
    }

    pub fn final_usage(&self) -> Option<super::cache::CacheUsage> {
        self.final_usage
    }

    pub fn final_reported_usage(&self) -> Option<super::cache::CacheUsage> {
        self.final_reported_usage
    }

    pub fn metadata_usage(&self) -> Option<&MetadataTokenUsage> {
        self.metadata_usage.as_ref()
    }

    pub fn context_input_tokens_seen(&self) -> bool {
        self.context_input_tokens.is_some()
    }

    pub fn stream_error_detail(&self) -> Option<(String, String)> {
        self.stream_error.clone()
    }

    fn create_error_event(error_type: String, message: String) -> SseEvent {
        SseEvent::new(
            "error",
            json!({
                "type": "error",
                "error": {
                    "type": error_type,
                    "message": message
                }
            }),
        )
    }

    /// 处理助手响应事件
    fn process_assistant_response(&mut self, content: &str) -> Vec<SseEvent> {
        if content.is_empty() {
            return Vec::new();
        }

        // 估算 tokens
        self.output_tokens += estimate_tokens(content);

        // 如果启用了thinking，需要处理thinking块
        if self.thinking_enabled && self.extract_xml_thinking && !self.native_reasoning_seen {
            return self.process_content_with_thinking(content);
        }

        // 非 thinking 模式同样复用统一的 text_delta 发送逻辑，
        // 以便在 tool_use 自动关闭文本块后能够自愈重建新的文本块，避免“吞字”。
        self.create_text_delta_events(content)
    }

    /// 处理原生 reasoningContentEvent。
    fn process_reasoning_content(
        &mut self,
        reasoning: &crate::kiro::model::events::ReasoningContentEvent,
    ) -> Vec<SseEvent> {
        if !self.thinking_enabled {
            return Vec::new();
        }

        self.native_reasoning_seen = true;
        let mut events = Vec::new();
        if let Some(signature) = reasoning.signature.as_deref() {
            if !signature.is_empty() {
                self.native_reasoning_signature = Some(signature.to_string());
            }
        }

        if let Some(redacted) = reasoning.redacted_content.as_deref() {
            if !redacted.is_empty() {
                let idx = self.state_manager.next_block_index();
                let start_events = self.state_manager.handle_content_block_start(
                    idx,
                    "redacted_thinking",
                    json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {
                            "type": "redacted_thinking",
                            "data": redacted
                        }
                    }),
                );
                events.extend(start_events);
                if let Some(stop_event) = self.state_manager.handle_content_block_stop(idx) {
                    events.push(stop_event);
                }
            }
            return events;
        }

        let text = reasoning.text.as_str();
        if text.is_empty() {
            return events;
        }

        let delta = if text.starts_with(&self.native_reasoning_content) {
            text[self.native_reasoning_content.len()..].to_string()
        } else {
            text.to_string()
        };
        self.native_reasoning_content = text.to_string();

        if delta.is_empty() {
            return events;
        }

        let thinking_index = if let Some(idx) = self.thinking_block_index {
            idx
        } else {
            let idx = self.state_manager.next_block_index();
            self.thinking_block_index = Some(idx);
            let content_block = json!({
                "type": "thinking",
                "thinking": ""
            });
            let start_events = self.state_manager.handle_content_block_start(
                idx,
                "thinking",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": content_block
                }),
            );
            events.extend(start_events);
            idx
        };

        events.push(self.create_thinking_delta_event(thinking_index, &delta));
        events
    }

    /// 处理包含thinking块的内容
    fn process_content_with_thinking(&mut self, content: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 将内容添加到缓冲区进行处理
        self.thinking_buffer.push_str(content);

        loop {
            if !self.in_thinking_block && !self.thinking_extracted {
                // 查找 <thinking> 开始标签（跳过被反引号包裹的）
                if let Some(start_pos) = find_real_thinking_start_tag(&self.thinking_buffer) {
                    // 发送 <thinking> 之前的内容作为 text_delta
                    // 注意：如果前面只是空白字符（如 adaptive 模式返回的 \n\n），则跳过，
                    // 避免在 thinking 块之前产生无意义的 text 块导致客户端解析失败
                    let before_thinking = self.thinking_buffer[..start_pos].to_string();
                    if !before_thinking.is_empty() && !before_thinking.trim().is_empty() {
                        events.extend(self.create_text_delta_events(&before_thinking));
                    }

                    // 进入 thinking 块
                    self.in_thinking_block = true;
                    self.strip_thinking_leading_newline = true;
                    self.thinking_buffer =
                        self.thinking_buffer[start_pos + "<thinking>".len()..].to_string();

                    // 创建 thinking 块的 content_block_start 事件
                    let thinking_index = self.state_manager.next_block_index();
                    self.thinking_block_index = Some(thinking_index);
                    let start_events = self.state_manager.handle_content_block_start(
                        thinking_index,
                        "thinking",
                        json!({
                            "type": "content_block_start",
                            "index": thinking_index,
                            "content_block": {
                                "type": "thinking",
                                "thinking": ""
                            }
                        }),
                    );
                    events.extend(start_events);
                } else {
                    // 没有找到 <thinking>，检查是否可能是部分标签
                    // 保留可能是部分标签的内容
                    let target_len = self
                        .thinking_buffer
                        .len()
                        .saturating_sub("<thinking>".len());
                    let safe_len = find_char_boundary(&self.thinking_buffer, target_len);
                    if safe_len > 0 {
                        let safe_content = self.thinking_buffer[..safe_len].to_string();
                        // 如果 thinking 尚未提取，且安全内容只是空白字符，
                        // 则不发送为 text_delta，继续保留在缓冲区等待更多内容。
                        // 这避免了 4.6 模型中 <thinking> 标签跨事件分割时，
                        // 前导空白（如 "\n\n"）被错误地创建为 text 块，
                        // 导致 text 块先于 thinking 块出现的问题。
                        if !safe_content.is_empty() && !safe_content.trim().is_empty() {
                            events.extend(self.create_text_delta_events(&safe_content));
                            self.thinking_buffer = self.thinking_buffer[safe_len..].to_string();
                        }
                    }
                    break;
                }
            } else if self.in_thinking_block {
                // 剥离 <thinking> 标签后紧跟的换行符（可能跨 chunk）
                if self.strip_thinking_leading_newline {
                    if self.thinking_buffer.starts_with('\n') {
                        self.thinking_buffer = self.thinking_buffer[1..].to_string();
                        self.strip_thinking_leading_newline = false;
                    } else if !self.thinking_buffer.is_empty() {
                        // buffer 非空但不以 \n 开头，不再需要剥离
                        self.strip_thinking_leading_newline = false;
                    }
                    // buffer 为空时保留标志，等待下一个 chunk
                }

                // 在 thinking 块内，查找 </thinking> 结束标签（跳过被反引号包裹的）
                if let Some(end_pos) = find_real_thinking_end_tag(&self.thinking_buffer) {
                    // 提取 thinking 内容
                    let thinking_content = self.thinking_buffer[..end_pos].to_string();
                    if !thinking_content.is_empty() {
                        if let Some(thinking_index) = self.thinking_block_index {
                            events.push(
                                self.create_thinking_delta_event(thinking_index, &thinking_content),
                            );
                        }
                    }

                    // 结束 thinking 块
                    self.in_thinking_block = false;
                    self.thinking_extracted = true;

                    // 发送空的 thinking_delta 事件，然后发送 content_block_stop 事件
                    if let Some(thinking_index) = self.thinking_block_index {
                        // 先发送空的 thinking_delta
                        events.push(self.create_thinking_delta_event(thinking_index, ""));
                        // 再发送 content_block_stop
                        if let Some(stop_event) =
                            self.state_manager.handle_content_block_stop(thinking_index)
                        {
                            events.push(stop_event);
                        }
                    }

                    // 剥离 `</thinking>\n\n`（find_real_thinking_end_tag 已确认 \n\n 存在）
                    self.thinking_buffer =
                        self.thinking_buffer[end_pos + "</thinking>\n\n".len()..].to_string();
                } else {
                    // 没有找到结束标签，发送当前缓冲区内容作为 thinking_delta。
                    // 保留末尾可能是部分 `</thinking>\n\n` 的内容：
                    // find_real_thinking_end_tag 要求标签后有 `\n\n` 才返回 Some，
                    // 因此保留区必须覆盖 `</thinking>\n\n` 的完整长度（13 字节），
                    // 否则当 `</thinking>` 已在 buffer 但 `\n\n` 尚未到达时，
                    // 标签的前几个字符会被错误地作为 thinking_delta 发出。
                    let target_len = self
                        .thinking_buffer
                        .len()
                        .saturating_sub("</thinking>\n\n".len());
                    let safe_len = find_char_boundary(&self.thinking_buffer, target_len);
                    if safe_len > 0 {
                        let safe_content = self.thinking_buffer[..safe_len].to_string();
                        if !safe_content.is_empty() {
                            if let Some(thinking_index) = self.thinking_block_index {
                                events.push(
                                    self.create_thinking_delta_event(thinking_index, &safe_content),
                                );
                            }
                        }
                        self.thinking_buffer = self.thinking_buffer[safe_len..].to_string();
                    }
                    break;
                }
            } else {
                // thinking 已提取完成，剩余内容作为 text_delta
                if !self.thinking_buffer.is_empty() {
                    let remaining = self.thinking_buffer.clone();
                    self.thinking_buffer.clear();
                    events.extend(self.create_text_delta_events(&remaining));
                }
                break;
            }
        }

        events
    }

    /// 创建 text_delta 事件（带字面 `<invoke>` 嗅探的统一明文出口）。
    fn create_text_delta_events(&mut self, text: &str) -> Vec<SseEvent> {
        if text.is_empty() {
            return Vec::new();
        }
        self.invoke_sniff_buffer.push_str(text);
        self.drain_invoke_sniff_buffer(false)
    }

    const MAX_INVOKE_HOLD_BYTES: usize = 262_144;

    fn drain_invoke_sniff_buffer(&mut self, flush: bool) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mut buf = std::mem::take(&mut self.invoke_sniff_buffer);

        loop {
            match find_invoke_start(&buf) {
                Some(start) => match find_invoke_block_end(&buf, start) {
                    Some(end) => {
                        let before = strip_trailing_tool_prefixes(&buf[..start]);
                        let fence_after_before = fence_open_after(
                            self.code_fence_open,
                            &self.fence_scan_partial,
                            before,
                        );
                        let parsed = parse_invoke_block(&buf[start..end]);
                        let name_known = parsed
                            .as_ref()
                            .map(|(name, _)| self.known_tool_names.contains(name))
                            .unwrap_or(false);

                        if invoke_looks_like_real_leak(before) && !fence_after_before && name_known
                        {
                            if !before.is_empty() {
                                events.extend(self.emit_text_delta_raw(before));
                            }
                            let (name, input_json) = parsed.expect("parsed is Some when known");
                            events.extend(self.queue_leaked_tool_use(name, input_json));
                            let close_len = leading_function_calls_close_len(&buf[end..]);
                            buf = buf[end + close_len..].to_string();
                        } else {
                            events.extend(self.emit_text_delta_raw(&buf[..end]));
                            buf = buf[end..].to_string();
                        }
                        continue;
                    }
                    None => {
                        let before = strip_trailing_tool_prefixes(&buf[..start]);
                        let fence_after_before = fence_open_after(
                            self.code_fence_open,
                            &self.fence_scan_partial,
                            before,
                        );
                        if !invoke_looks_like_real_leak(before) || fence_after_before {
                            if !buf.is_empty() {
                                events.extend(self.emit_text_delta_raw(&buf));
                            }
                            break;
                        }
                        if start > 0 && !before.is_empty() {
                            events.extend(self.emit_text_delta_raw(before));
                        }
                        let remainder = buf[start..].to_string();
                        if flush || remainder.len() > Self::MAX_INVOKE_HOLD_BYTES {
                            events.extend(self.emit_text_delta_raw(&remainder));
                        } else {
                            self.invoke_sniff_buffer = remainder;
                        }
                        break;
                    }
                },
                None => {
                    if flush {
                        if !buf.is_empty() {
                            events.extend(self.emit_text_delta_raw(&buf));
                        }
                    } else {
                        let keep = trailing_tool_protocol_tail_len(&buf);
                        let split = buf.len().saturating_sub(keep);
                        let safe = find_char_boundary(&buf, split);
                        if safe > 0 {
                            events.extend(self.emit_text_delta_raw(&buf[..safe]));
                        }
                        self.invoke_sniff_buffer = buf[safe..].to_string();
                    }
                    break;
                }
            }
        }

        events
    }

    fn queue_leaked_tool_use(&mut self, parsed_name: String, input_json: String) -> Vec<SseEvent> {
        let output_name = self
            .tool_name_map
            .get(&parsed_name)
            .cloned()
            .unwrap_or(parsed_name);
        let sig = tool_use_signature_from_json_str(&output_name, &input_json);
        if self.seen_tool_sigs.contains(&sig) {
            tracing::debug!(tool = %output_name, "跳过重复的字面 invoke 工具调用");
            return Vec::new();
        }
        if self
            .pending_leaked_tools
            .iter()
            .any(|(_, _, pending_sig)| pending_sig == &sig)
        {
            return Vec::new();
        }
        self.pending_leaked_tools
            .push((output_name, input_json, sig));
        Vec::new()
    }

    fn emit_queued_leaked_tool_uses(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let pending = std::mem::take(&mut self.pending_leaked_tools);
        for (output_name, input_json, sig) in pending {
            if self.seen_tool_sigs.contains(&sig) {
                tracing::debug!(tool = %output_name, "流末尾跳过重复的泄漏 tool_use");
                continue;
            }
            events.extend(self.emit_synthesized_tool_use(output_name, input_json, sig));
        }
        events
    }

    fn emit_synthesized_tool_use(
        &mut self,
        output_name: String,
        input_json: String,
        sig: String,
    ) -> Vec<SseEvent> {
        self.seen_tool_sigs.insert(sig);

        let mut events = Vec::new();
        self.state_manager.set_has_tool_use(true);
        let block_index = self.state_manager.next_block_index();
        let tool_use_id = format!("toolu_{}", Uuid::new_v4().to_string().replace('-', ""));
        self.tool_block_indices
            .insert(tool_use_id.clone(), block_index);
        events.extend(self.state_manager.handle_content_block_start(
            block_index,
            "tool_use",
            json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": output_name,
                    "input": {}
                }
            }),
        ));
        if !input_json.is_empty() {
            if let Some(delta_event) = self.state_manager.handle_content_block_delta(
                block_index,
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": input_json
                    }
                }),
            ) {
                events.push(delta_event);
            }
        }
        if let Some(stop_event) = self.state_manager.handle_content_block_stop(block_index) {
            events.push(stop_event);
        }
        events
    }

    fn repeat_guard_filter(&mut self, text: &str) -> String {
        if self.repeat_guard_tripped {
            return String::new();
        }

        let mut kept = String::new();
        for segment in text.split_inclusive('\n') {
            let line = segment.trim();
            if STRAY_INVOKE_TOKENS.contains(&line) {
                if line == self.repeat_guard_last_line {
                    self.repeat_guard_run += 1;
                } else {
                    self.repeat_guard_last_line = line.to_string();
                    self.repeat_guard_run = 1;
                }
                if self.repeat_guard_run >= REPEAT_GUARD_TRIP_THRESHOLD {
                    self.repeat_guard_tripped = true;
                    return kept;
                }
                kept.push_str(segment);
            } else {
                if !line.is_empty() {
                    self.repeat_guard_last_line = line.to_string();
                    self.repeat_guard_run = 0;
                }
                kept.push_str(segment);
            }
        }
        kept
    }

    /// 原始 text_delta 发送逻辑；调用前必须已经完成 invoke 嗅探。
    fn emit_text_delta_raw(&mut self, text: &str) -> Vec<SseEvent> {
        let kept = self.repeat_guard_filter(text);
        if kept.is_empty() {
            return Vec::new();
        }
        let text = kept.as_str();
        let mut fence_open = self.code_fence_open;
        let mut fence_partial = std::mem::take(&mut self.fence_scan_partial);
        advance_code_fence_state(&mut fence_open, &mut fence_partial, text);
        self.code_fence_open = fence_open;
        self.fence_scan_partial = fence_partial;

        let mut events = Vec::new();

        // 如果当前 text_block_index 指向的块已经被关闭（例如 tool_use 开始时自动 stop），
        // 则丢弃该索引并创建新的文本块继续输出，避免 delta 被状态机拒绝导致“吞字”。
        if let Some(idx) = self.text_block_index {
            if !self.state_manager.is_block_open_of_type(idx, "text") {
                self.text_block_index = None;
            }
        }

        // 获取或创建文本块索引
        let text_index = if let Some(idx) = self.text_block_index {
            idx
        } else {
            // 文本块尚未创建，需要先创建
            let idx = self.state_manager.next_block_index();
            self.text_block_index = Some(idx);

            // 发送 content_block_start 事件
            let start_events = self.state_manager.handle_content_block_start(
                idx,
                "text",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {
                        "type": "text",
                        "text": ""
                    }
                }),
            );
            events.extend(start_events);
            idx
        };

        // 发送 content_block_delta 事件
        if let Some(delta_event) = self.state_manager.handle_content_block_delta(
            text_index,
            json!({
                "type": "content_block_delta",
                "index": text_index,
                "delta": {
                    "type": "text_delta",
                    "text": text
                }
            }),
        ) {
            events.push(delta_event);
        }

        events
    }

    /// 创建 thinking_delta 事件
    fn create_thinking_delta_event(&self, index: i32, thinking: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": thinking
                }
            }),
        )
    }

    /// 创建官方 Anthropic extended-thinking 签名 delta。
    fn create_signature_delta_event(&self, index: i32, signature: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "signature_delta",
                    "signature": signature
                }
            }),
        )
    }

    fn take_native_signature_delta_event(&mut self, index: i32) -> Option<SseEvent> {
        if self.native_reasoning_signature_sent {
            return None;
        }

        let signature = self.native_reasoning_signature.as_ref()?.clone();
        if signature.is_empty() {
            return None;
        }

        self.native_reasoning_signature_sent = true;
        Some(self.create_signature_delta_event(index, &signature))
    }

    fn close_native_reasoning_block(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if let Some(thinking_index) = self.thinking_block_index {
            if let Some(signature_event) = self.take_native_signature_delta_event(thinking_index) {
                events.push(signature_event);
            }
            if let Some(stop_event) = self.state_manager.handle_content_block_stop(thinking_index) {
                events.push(stop_event);
            }
        }
        events
    }

    /// 处理工具使用事件
    fn process_tool_use(
        &mut self,
        tool_use: &crate::kiro::model::events::ToolUseEvent,
    ) -> Vec<SseEvent> {
        let mut events = Vec::new();

        self.state_manager.set_has_tool_use(true);

        if self.native_reasoning_seen {
            events.extend(self.close_native_reasoning_block());
            self.thinking_buffer.clear();
        }

        // tool_use 必须发生在 thinking 结束之后。
        // 但当 `</thinking>` 后面没有 `\n\n`（例如紧跟 tool_use 或流结束）时，
        // thinking 结束标签会滞留在 thinking_buffer，导致后续 flush 时把 `</thinking>` 当作内容输出。
        // 这里在开始 tool_use block 前做一次“边界场景”的结束标签识别与过滤。
        if self.thinking_enabled && self.extract_xml_thinking && self.in_thinking_block {
            if let Some(end_pos) = find_real_thinking_end_tag_at_buffer_end(&self.thinking_buffer) {
                let thinking_content = self.thinking_buffer[..end_pos].to_string();
                if !thinking_content.is_empty() {
                    if let Some(thinking_index) = self.thinking_block_index {
                        events.push(
                            self.create_thinking_delta_event(thinking_index, &thinking_content),
                        );
                    }
                }

                // 结束 thinking 块
                self.in_thinking_block = false;
                self.thinking_extracted = true;

                if let Some(thinking_index) = self.thinking_block_index {
                    // 先发送空的 thinking_delta
                    events.push(self.create_thinking_delta_event(thinking_index, ""));
                    // 再发送 content_block_stop
                    if let Some(stop_event) =
                        self.state_manager.handle_content_block_stop(thinking_index)
                    {
                        events.push(stop_event);
                    }
                }

                // 把结束标签后的内容当作普通文本（通常为空或空白）
                let after_pos = end_pos + "</thinking>".len();
                let remaining = self.thinking_buffer[after_pos..].trim_start().to_string();
                self.thinking_buffer.clear();
                if !remaining.is_empty() {
                    events.extend(self.create_text_delta_events(&remaining));
                }
            }
        }

        // thinking 模式下，process_content_with_thinking 可能会为了探测 `<thinking>` 而暂存一小段尾部文本。
        // 如果此时直接开始 tool_use，状态机会自动关闭 text block，导致这段"待输出文本"看起来被 tool_use 吞掉。
        // 约束：只在尚未进入 thinking block、且 thinking 尚未被提取时，将缓冲区当作普通文本 flush。
        if self.thinking_enabled
            && self.extract_xml_thinking
            && !self.in_thinking_block
            && !self.thinking_extracted
            && !self.thinking_buffer.is_empty()
        {
            let buffered = std::mem::take(&mut self.thinking_buffer);
            events.extend(self.create_text_delta_events(&buffered));
        }

        // 获取或分配块索引
        let block_index = if let Some(&idx) = self.tool_block_indices.get(&tool_use.tool_use_id) {
            idx
        } else {
            let idx = self.state_manager.next_block_index();
            self.tool_block_indices
                .insert(tool_use.tool_use_id.clone(), idx);
            idx
        };

        // 还原工具名称（如果有映射）
        let original_name = self
            .tool_name_map
            .get(&tool_use.name)
            .cloned()
            .unwrap_or_else(|| tool_use.name.clone());

        if !tool_use.input.is_empty() {
            self.tool_input_buffers
                .entry(tool_use.tool_use_id.clone())
                .or_default()
                .push_str(&tool_use.input);
        }

        // 发送 content_block_start
        let start_events = self.state_manager.handle_content_block_start(
            block_index,
            "tool_use",
            json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_use.tool_use_id,
                    "name": original_name,
                    "input": {}
                }
            }),
        );
        events.extend(start_events);

        // 发送参数增量 (ToolUseEvent.input 是 String 类型)
        if !tool_use.input.is_empty() {
            self.output_tokens += (tool_use.input.len() as i32 + 3) / 4; // 估算 token

            if let Some(delta_event) = self.state_manager.handle_content_block_delta(
                block_index,
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": tool_use.input
                    }
                }),
            ) {
                events.push(delta_event);
            }
        }

        // 如果是完整的工具调用（stop=true），发送 content_block_stop
        if tool_use.stop {
            let full_input = self
                .tool_input_buffers
                .remove(&tool_use.tool_use_id)
                .unwrap_or_else(|| tool_use.input.clone());
            let sig = tool_use_signature_from_json_str(&original_name, &full_input);
            self.seen_tool_sigs.insert(sig);
            if let Some(stop_event) = self.state_manager.handle_content_block_stop(block_index) {
                events.push(stop_event);
            }
        }

        events
    }

    /// 生成最终事件序列
    pub fn generate_final_events(&mut self) -> Vec<SseEvent> {
        self.generate_final_events_with_reported_usage_mapper(|_, reported_usage, _, _| {
            reported_usage
        })
    }

    pub fn generate_final_events_with_reported_usage_mapper<F>(
        &mut self,
        usage_mapper: F,
    ) -> Vec<SseEvent>
    where
        F: FnOnce(
            super::cache::CacheUsage,
            super::cache::CacheUsage,
            Option<&MetadataTokenUsage>,
            bool,
        ) -> super::cache::CacheUsage,
    {
        let mut events = Vec::new();

        if self.native_reasoning_seen {
            events.extend(self.close_native_reasoning_block());
            self.thinking_buffer.clear();
            self.in_thinking_block = false;
            self.thinking_extracted = true;
        }

        // Flush thinking_buffer 中的剩余内容
        if self.thinking_enabled && self.extract_xml_thinking && !self.thinking_buffer.is_empty() {
            if self.in_thinking_block {
                // 末尾可能残留 `</thinking>`（例如紧跟 tool_use 或流结束），需要在 flush 时过滤掉结束标签。
                if let Some(end_pos) =
                    find_real_thinking_end_tag_at_buffer_end(&self.thinking_buffer)
                {
                    let thinking_content = self.thinking_buffer[..end_pos].to_string();
                    if !thinking_content.is_empty() {
                        if let Some(thinking_index) = self.thinking_block_index {
                            events.push(
                                self.create_thinking_delta_event(thinking_index, &thinking_content),
                            );
                        }
                    }

                    // 关闭 thinking 块：先发送空的 thinking_delta，再发送 content_block_stop
                    if let Some(thinking_index) = self.thinking_block_index {
                        events.push(self.create_thinking_delta_event(thinking_index, ""));
                        if let Some(stop_event) =
                            self.state_manager.handle_content_block_stop(thinking_index)
                        {
                            events.push(stop_event);
                        }
                    }

                    // 把结束标签后的内容当作普通文本（通常为空或空白）
                    let after_pos = end_pos + "</thinking>".len();
                    let remaining = self.thinking_buffer[after_pos..].trim_start().to_string();
                    self.thinking_buffer.clear();
                    self.in_thinking_block = false;
                    self.thinking_extracted = true;
                    if !remaining.is_empty() {
                        events.extend(self.create_text_delta_events(&remaining));
                    }
                } else {
                    // 如果还在 thinking 块内，发送剩余内容作为 thinking_delta
                    if let Some(thinking_index) = self.thinking_block_index {
                        events.push(
                            self.create_thinking_delta_event(thinking_index, &self.thinking_buffer),
                        );
                    }
                    // 关闭 thinking 块：先发送空的 thinking_delta，再发送 content_block_stop
                    if let Some(thinking_index) = self.thinking_block_index {
                        // 先发送空的 thinking_delta
                        events.push(self.create_thinking_delta_event(thinking_index, ""));
                        // 再发送 content_block_stop
                        if let Some(stop_event) =
                            self.state_manager.handle_content_block_stop(thinking_index)
                        {
                            events.push(stop_event);
                        }
                    }
                }
            } else {
                // 否则发送剩余内容作为 text_delta
                let buffer_content = self.thinking_buffer.clone();
                events.extend(self.create_text_delta_events(&buffer_content));
            }
            self.thinking_buffer.clear();
        }

        if let Some((error_type, message)) = self.stream_error.take() {
            events.extend(self.state_manager.close_open_blocks());
            events.push(Self::create_error_event(error_type, message));
            return events;
        }

        // 如果整个流中只产生了 thinking 块，没有 text 也没有 tool_use，
        // 则设置 stop_reason 为 max_tokens（表示模型耗尽了 token 预算在思考上），
        // 并补发一套完整的 text 事件（内容为一个空格），确保 content 数组中有 text 块
        if self.thinking_enabled
            && self.extract_xml_thinking
            && self.thinking_block_index.is_some()
            && !self.state_manager.has_non_thinking_blocks()
        {
            self.state_manager.set_stop_reason("max_tokens");
            events.extend(self.create_text_delta_events(" "));
        }

        if !self.invoke_sniff_buffer.is_empty() {
            events.extend(self.drain_invoke_sniff_buffer(true));
        }
        events.extend(self.emit_queued_leaked_tool_uses());

        // 优先使用 metadataEvent 的准确 token usage；缺失时回退到 contextUsageEvent 估算值。
        let final_input_tokens = self
            .metadata_usage
            .as_ref()
            .map(|usage| usage.total_input_tokens())
            .or(self.context_input_tokens)
            .unwrap_or(self.input_tokens);
        let usage_input_tokens = if self.simulation_mode == PromptCacheSimulationMode::HighCache {
            final_input_tokens.max(self.input_tokens)
        } else {
            final_input_tokens
        };
        let final_output_tokens = self
            .metadata_usage
            .as_ref()
            .map(|usage| usage.output_tokens)
            .unwrap_or(self.output_tokens);
        let final_usage = super::cache::build_usage_with_simulation_policy(
            self.metadata_usage.as_ref(),
            usage_input_tokens,
            final_output_tokens,
            self.simulated_usage,
            self.simulation_mode == PromptCacheSimulationMode::HighCache,
        );
        self.final_usage = Some(final_usage);
        let reported_usage = self.reported_usage_for_downstream(final_usage);
        let context_estimated =
            self.metadata_usage.is_none() && self.context_input_tokens.is_some();
        let reported_usage = usage_mapper(
            final_usage,
            reported_usage,
            self.metadata_usage.as_ref(),
            context_estimated,
        );
        self.final_reported_usage = Some(reported_usage);

        // 生成最终事件
        events.extend(
            self.state_manager
                .generate_final_events_with_usage(reported_usage.to_anthropic_usage_json()),
        );
        events
    }
}

/// 简单的 token 估算
fn estimate_tokens(text: &str) -> i32 {
    let chars: Vec<char> = text.chars().collect();
    let mut chinese_count = 0;
    let mut other_count = 0;

    for c in &chars {
        if *c >= '\u{4E00}' && *c <= '\u{9FFF}' {
            chinese_count += 1;
        } else {
            other_count += 1;
        }
    }

    // 中文约 1.5 字符/token，英文约 4 字符/token
    let chinese_tokens = (chinese_count * 2 + 2) / 3;
    let other_tokens = (other_count + 3) / 4;

    (chinese_tokens + other_tokens).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_event_format() {
        let event = SseEvent::new("message_start", json!({"type": "message_start"}));
        let sse_str = event.to_sse_string();

        assert!(sse_str.starts_with("event: message_start\n"));
        assert!(sse_str.contains("data: "));
        assert!(sse_str.ends_with("\n\n"));
    }

    #[test]
    fn test_sse_state_manager_message_start() {
        let mut manager = SseStateManager::new();

        // 第一次应该成功
        let event = manager.handle_message_start(json!({"type": "message_start"}));
        assert!(event.is_some());

        // 第二次应该被跳过
        let event = manager.handle_message_start(json!({"type": "message_start"}));
        assert!(event.is_none());
    }

    #[test]
    fn test_sse_state_manager_block_lifecycle() {
        let mut manager = SseStateManager::new();

        // 创建块
        let events = manager.handle_content_block_start(0, "text", json!({}));
        assert_eq!(events.len(), 1);

        // delta
        let event = manager.handle_content_block_delta(0, json!({}));
        assert!(event.is_some());

        // stop
        let event = manager.handle_content_block_stop(0);
        assert!(event.is_some());

        // 重复 stop 应该被跳过
        let event = manager.handle_content_block_stop(0);
        assert!(event.is_none());
    }

    #[test]
    fn test_tool_name_reverse_mapping_in_stream() {
        use crate::kiro::model::events::ToolUseEvent;

        let mut map = HashMap::new();
        map.insert(
            "short_abc12345".to_string(),
            "mcp__very_long_original_tool_name".to_string(),
        );

        let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, map);
        let _ = ctx.generate_initial_events();

        // 模拟 Kiro 返回短名称的 tool_use
        let tool_event = Event::ToolUse(ToolUseEvent {
            name: "short_abc12345".to_string(),
            tool_use_id: "toolu_01".to_string(),
            input: r#"{"key":"value"}"#.to_string(),
            stop: true,
        });

        let events = ctx.process_kiro_event(&tool_event);

        // content_block_start 中的 name 应该是原始长名称
        let start_event = events
            .iter()
            .find(|e| e.event == "content_block_start")
            .unwrap();
        assert_eq!(
            start_event.data["content_block"]["name"], "mcp__very_long_original_tool_name",
            "应还原为原始工具名称"
        );
    }

    #[test]
    fn test_native_reasoning_content_uses_cumulative_deltas() {
        use crate::kiro::model::events::ReasoningContentEvent;

        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_kiro_event(&Event::ReasoningContent(
            ReasoningContentEvent {
                text: "hello".to_string(),
                signature: Some("sig".to_string()),
                redacted_content: None,
            },
        )));
        all_events.extend(ctx.process_kiro_event(&Event::ReasoningContent(
            ReasoningContentEvent {
                text: "hello world".to_string(),
                signature: Some("sig".to_string()),
                redacted_content: None,
            },
        )));
        all_events.extend(ctx.process_assistant_response("answer"));
        all_events.extend(ctx.generate_final_events());

        assert_eq!(collect_thinking_content(&all_events), "hello world");
        assert_eq!(collect_text_content(&all_events), "answer");

        let thinking_start = all_events
            .iter()
            .find(|e| {
                e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking"
            })
            .expect("native thinking block should start");
        assert!(thinking_start.data["content_block"]["signature"].is_null());

        let signature_delta_pos = all_events
            .iter()
            .position(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "signature_delta"
                    && e.data["delta"]["signature"] == "sig"
            })
            .expect("native thinking signature should be emitted as signature_delta");
        let thinking_stop_pos = all_events
            .iter()
            .position(|e| {
                e.event == "content_block_stop"
                    && e.data["index"].as_i64() == thinking_start.data["index"].as_i64()
            })
            .expect("native thinking block should stop");
        assert!(
            signature_delta_pos < thinking_stop_pos,
            "signature_delta must be emitted before content_block_stop"
        );
    }

    #[test]
    fn test_xml_thinking_extraction_can_be_disabled_for_strict_profile() {
        let mut ctx = StreamContext::new_with_simulation(
            "test-model",
            1,
            DEFAULT_CONTEXT_WINDOW_TOKENS,
            true,
            false,
            HashMap::new(),
            None,
            PromptCacheSimulationMode::Disabled,
        );

        let mut events = ctx.process_assistant_response("<thinking>secret</thinking>\n\nvisible");
        events.extend(ctx.generate_final_events());

        assert!(
            events.iter().all(|e| {
                !(e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking")
            }),
            "unsigned XML thinking should not be exposed as thinking block"
        );
        assert!(
            events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("<thinking>secret</thinking>"))
            }),
            "XML thinking tags should remain ordinary text when extraction is disabled"
        );
    }

    #[test]
    fn test_metadata_usage_overrides_final_usage() {
        use crate::kiro::model::events::{MetadataEvent, MetadataTokenUsage};

        let mut ctx = StreamContext::new_with_thinking("test-model", 12, false, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("hello"));
        all_events.extend(ctx.process_kiro_event(&Event::Metadata(MetadataEvent {
            token_usage: Some(MetadataTokenUsage {
                uncached_input_tokens: 100,
                output_tokens: 9,
                total_tokens: 116,
                cache_read_input_tokens: 7,
                cache_write_input_tokens: 3,
            }),
        })));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("message_delta should exist");
        assert_eq!(message_delta.data["usage"]["input_tokens"], 100);
        assert_eq!(message_delta.data["usage"]["output_tokens"], 9);
    }

    #[test]
    fn test_stream_usage_is_sub2api_compatible() {
        let mut ctx = StreamContext::new_with_simulation(
            "test-model",
            100_000,
            200_000,
            false,
            true,
            HashMap::new(),
            Some(crate::anthropic::cache::CacheSimulation {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 50_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                target_cache_ratio: None,
                amplification: None,
            }),
            PromptCacheSimulationMode::HighCache,
        );
        ctx.set_reported_cache_usage_policy(
            crate::anthropic::cache::ReportedCacheUsagePolicy::new(3_000, 96, 7),
        );

        let initial_events = ctx.generate_initial_events();
        let message_start = initial_events
            .iter()
            .find(|e| e.event == "message_start")
            .expect("message_start should exist");
        let start_usage = &message_start.data["message"]["usage"];
        assert_eq!(start_usage["input_tokens"], 0);
        assert_eq!(start_usage["output_tokens"], 0);
        assert_eq!(start_usage["cache_creation_input_tokens"], 0);
        assert_eq!(start_usage["cache_read_input_tokens"], 0);
        assert!(start_usage.get("cache_creation_5m_input_tokens").is_none());
        assert!(start_usage.get("cache_creation_1h_input_tokens").is_none());

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("hello"));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("message_delta should exist");
        let final_usage = &message_delta.data["usage"];
        let final_input = final_usage["input_tokens"].as_i64().expect("input");
        let final_output = final_usage["output_tokens"].as_i64().expect("output");
        let final_cache_creation = final_usage["cache_creation_input_tokens"]
            .as_i64()
            .expect("cache creation");
        let final_cache_read = final_usage["cache_read_input_tokens"]
            .as_i64()
            .expect("cache read");

        assert!((1..=96).contains(&final_input));
        assert!(final_output > 0);
        assert_eq!(final_usage["cache_creation_input_tokens"], 0);
        assert!(final_cache_read > 50_000);
        assert!(final_usage.get("cache_creation_5m_input_tokens").is_none());
        assert!(final_usage.get("cache_creation_1h_input_tokens").is_none());

        let mut sub2api_input = start_usage["input_tokens"].as_i64().unwrap_or_default();
        let mut sub2api_output = start_usage["output_tokens"].as_i64().unwrap_or_default();
        let mut sub2api_cache_creation = start_usage["cache_creation_input_tokens"]
            .as_i64()
            .unwrap_or_default();
        let mut sub2api_cache_read = start_usage["cache_read_input_tokens"]
            .as_i64()
            .unwrap_or_default();

        if final_input > 0 {
            sub2api_input = final_input;
        }
        if final_output > 0 {
            sub2api_output = final_output;
        }
        if final_cache_creation > 0 {
            sub2api_cache_creation = final_cache_creation;
        }
        if final_cache_read > 0 {
            sub2api_cache_read = final_cache_read;
        }

        assert_eq!(sub2api_input, final_input);
        assert_eq!(sub2api_output, final_output);
        assert_eq!(sub2api_cache_creation, final_cache_creation);
        assert_eq!(sub2api_cache_read, final_cache_read);
    }

    #[test]
    fn test_message_metadata_usage_overrides_final_usage() {
        use crate::kiro::model::events::{MessageMetadataEvent, MetadataTokenUsage};

        let mut ctx = StreamContext::new_with_thinking("test-model", 12, false, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("hello"));
        all_events.extend(
            ctx.process_kiro_event(&Event::MessageMetadata(MessageMetadataEvent {
                conversation_id: Some("conv-1".to_string()),
                utterance_id: Some("utt-1".to_string()),
                token_usage: Some(MetadataTokenUsage {
                    uncached_input_tokens: 21,
                    output_tokens: 13,
                    total_tokens: 377,
                    cache_read_input_tokens: 300,
                    cache_write_input_tokens: 43,
                }),
            })),
        );
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("message_delta should exist");
        assert_eq!(message_delta.data["usage"]["input_tokens"], 21);
        assert_eq!(message_delta.data["usage"]["cache_read_input_tokens"], 300);
        assert_eq!(
            message_delta.data["usage"]["cache_creation_input_tokens"],
            43
        );
        assert_eq!(message_delta.data["usage"]["output_tokens"], 13);
    }

    #[test]
    fn test_context_usage_percentage_uses_catalog_window_for_final_usage() {
        use crate::kiro::model::events::ContextUsageEvent;

        let mut ctx = StreamContext::new_with_simulation(
            "claude-sonnet-4.5",
            1_000,
            200_000,
            false,
            true,
            HashMap::new(),
            None,
            PromptCacheSimulationMode::Disabled,
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("ok"));
        all_events.extend(
            ctx.process_kiro_event(&Event::ContextUsage(ContextUsageEvent {
                context_usage_percentage: 12.5,
            })),
        );
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|event| event.event == "message_delta")
            .expect("message_delta should exist");
        assert_eq!(ctx.context_input_tokens, Some(25_000));
        assert_eq!(message_delta.data["usage"]["input_tokens"], 25_000);
        assert_eq!(message_delta.data["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn test_context_usage_100_percent_reports_context_window_exceeded() {
        use crate::kiro::model::events::ContextUsageEvent;

        let mut ctx = StreamContext::new_with_simulation(
            "claude-sonnet-4.6",
            1_000,
            1_000_000,
            false,
            true,
            HashMap::new(),
            None,
            PromptCacheSimulationMode::Disabled,
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("near limit"));
        all_events.extend(
            ctx.process_kiro_event(&Event::ContextUsage(ContextUsageEvent {
                context_usage_percentage: 100.0,
            })),
        );
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|event| event.event == "message_delta")
            .expect("message_delta should exist");
        assert_eq!(ctx.context_input_tokens, Some(1_000_000));
        assert_eq!(message_delta.data["usage"]["input_tokens"], 1_000_000);
        assert_eq!(
            message_delta.data["delta"]["stop_reason"],
            "model_context_window_exceeded"
        );
    }

    #[test]
    fn test_code_event_is_forwarded_as_text_content() {
        use crate::kiro::model::events::CodeEvent;

        let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_kiro_event(&Event::Code(CodeEvent {
            content: "let value = 1;".to_string(),
        }));

        assert!(events.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"] == "let value = 1;"
        }));
    }

    #[test]
    fn test_high_cache_metadata_usage_is_preserved_in_final_usage() {
        use crate::kiro::model::events::{MetadataEvent, MetadataTokenUsage};

        let mut ctx = StreamContext::new_with_thinking("test-model", 12, false, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("hello"));
        all_events.extend(ctx.process_kiro_event(&Event::Metadata(MetadataEvent {
            token_usage: Some(MetadataTokenUsage {
                uncached_input_tokens: 1200,
                output_tokens: 900,
                total_tokens: 206100,
                cache_read_input_tokens: 180000,
                cache_write_input_tokens: 24000,
            }),
        })));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("message_delta should exist");
        assert_eq!(message_delta.data["usage"]["input_tokens"], 1200);
        assert_eq!(message_delta.data["usage"]["output_tokens"], 900);
    }

    #[test]
    fn test_invalid_state_finishes_with_error_not_message_stop() {
        use crate::kiro::model::events::InvalidStateEvent;

        let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("partial"));
        all_events.extend(
            ctx.process_kiro_event(&Event::InvalidState(InvalidStateEvent {
                reason: "Expired".to_string(),
                message: "session expired".to_string(),
            })),
        );
        all_events.extend(ctx.generate_final_events());

        assert!(
            all_events.iter().any(|e| e.event == "content_block_stop"),
            "open text block should be closed before error"
        );
        assert!(
            all_events
                .iter()
                .all(|e| e.event != "message_delta" && e.event != "message_stop"),
            "error streams should not also emit normal message_delta/message_stop"
        );
        let error = all_events
            .iter()
            .find(|e| e.event == "error")
            .expect("error event should be emitted");
        assert_eq!(error.data["error"]["type"], "invalid_request_error");
        assert_eq!(error.data["error"]["message"], "session expired");
    }

    #[test]
    fn test_text_delta_after_tool_use_restarts_text_block() {
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());

        let initial_events = ctx.generate_initial_events();
        assert!(
            initial_events
                .iter()
                .any(|e| e.event == "content_block_start"
                    && e.data["content_block"]["type"] == "text")
        );

        let initial_text_index = ctx
            .text_block_index
            .expect("initial text block index should exist");

        // tool_use 开始会自动关闭现有 text block
        let tool_events = ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
            name: "test_tool".to_string(),
            tool_use_id: "tool_1".to_string(),
            input: "{}".to_string(),
            stop: false,
        });
        assert!(
            tool_events.iter().any(|e| {
                e.event == "content_block_stop"
                    && e.data["index"].as_i64() == Some(initial_text_index as i64)
            }),
            "tool_use should stop the previous text block"
        );

        // 之后再来文本增量，应自动创建新的 text block 而不是往已 stop 的块里写 delta
        let text_events = ctx.process_assistant_response("hello");
        let new_text_start_index = text_events.iter().find_map(|e| {
            if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                e.data["index"].as_i64()
            } else {
                None
            }
        });
        assert!(
            new_text_start_index.is_some(),
            "should start a new text block"
        );
        assert_ne!(
            new_text_start_index.unwrap(),
            initial_text_index as i64,
            "new text block index should differ from the stopped one"
        );
        assert!(
            text_events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"] == "hello"
            }),
            "should emit text_delta after restarting text block"
        );
    }

    #[test]
    fn test_tool_use_flushes_pending_thinking_buffer_text_before_tool_block() {
        // thinking 模式下，短文本可能被暂存在 thinking_buffer 以等待 `<thinking>` 的跨 chunk 匹配。
        // 当紧接着出现 tool_use 时，应先 flush 这段文本，再开始 tool_use block。
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        // 两段短文本（各 2 个中文字符），总长度仍可能不足以满足 safe_len>0 的输出条件，
        // 因而会留在 thinking_buffer 中等待后续 chunk。
        let ev1 = ctx.process_assistant_response("有修");
        assert!(
            ev1.iter().all(|e| e.event != "content_block_delta"),
            "short prefix should be buffered under thinking mode"
        );
        let ev2 = ctx.process_assistant_response("改：");
        assert!(
            ev2.iter().all(|e| e.event != "content_block_delta"),
            "short prefix should still be buffered under thinking mode"
        );

        let events = ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
            name: "Write".to_string(),
            tool_use_id: "tool_1".to_string(),
            input: "{}".to_string(),
            stop: false,
        });

        let text_start_index = events.iter().find_map(|e| {
            if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                e.data["index"].as_i64()
            } else {
                None
            }
        });
        let pos_text_delta = events.iter().position(|e| {
            e.event == "content_block_delta" && e.data["delta"]["type"] == "text_delta"
        });
        let pos_text_stop = text_start_index.and_then(|idx| {
            events.iter().position(|e| {
                e.event == "content_block_stop" && e.data["index"].as_i64() == Some(idx)
            })
        });
        let pos_tool_start = events.iter().position(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
        });

        assert!(
            text_start_index.is_some(),
            "should start a text block to flush buffered text"
        );
        assert!(
            pos_text_delta.is_some(),
            "should flush buffered text as text_delta"
        );
        assert!(
            pos_text_stop.is_some(),
            "should stop text block before tool_use block starts"
        );
        assert!(pos_tool_start.is_some(), "should start tool_use block");

        let pos_text_delta = pos_text_delta.unwrap();
        let pos_text_stop = pos_text_stop.unwrap();
        let pos_tool_start = pos_tool_start.unwrap();

        assert!(
            pos_text_delta < pos_text_stop && pos_text_stop < pos_tool_start,
            "ordering should be: text_delta -> text_stop -> tool_use_start"
        );

        assert!(
            events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"] == "有修改："
            }),
            "flushed text should equal the buffered prefix"
        );
    }

    #[test]
    fn test_estimate_tokens() {
        assert!(estimate_tokens("Hello") > 0);
        assert!(estimate_tokens("你好") > 0);
        assert!(estimate_tokens("Hello 你好") > 0);
    }

    #[test]
    fn test_find_real_thinking_start_tag_basic() {
        // 基本情况：正常的开始标签
        assert_eq!(find_real_thinking_start_tag("<thinking>"), Some(0));
        assert_eq!(find_real_thinking_start_tag("prefix<thinking>"), Some(6));
    }

    #[test]
    fn test_find_real_thinking_start_tag_with_backticks() {
        // 被反引号包裹的应该被跳过
        assert_eq!(find_real_thinking_start_tag("`<thinking>`"), None);
        assert_eq!(find_real_thinking_start_tag("use `<thinking>` tag"), None);

        // 先有被包裹的，后有真正的开始标签
        assert_eq!(
            find_real_thinking_start_tag("about `<thinking>` tag<thinking>content"),
            Some(22)
        );
    }

    #[test]
    fn test_find_real_thinking_start_tag_with_quotes() {
        // 被双引号包裹的应该被跳过
        assert_eq!(find_real_thinking_start_tag("\"<thinking>\""), None);
        assert_eq!(find_real_thinking_start_tag("the \"<thinking>\" tag"), None);

        // 被单引号包裹的应该被跳过
        assert_eq!(find_real_thinking_start_tag("'<thinking>'"), None);

        // 混合情况
        assert_eq!(
            find_real_thinking_start_tag("about \"<thinking>\" and '<thinking>' then<thinking>"),
            Some(40)
        );
    }

    #[test]
    fn test_find_real_thinking_end_tag_basic() {
        // 基本情况：正常的结束标签后面有双换行符
        assert_eq!(find_real_thinking_end_tag("</thinking>\n\n"), Some(0));
        assert_eq!(
            find_real_thinking_end_tag("content</thinking>\n\n"),
            Some(7)
        );
        assert_eq!(
            find_real_thinking_end_tag("some text</thinking>\n\nmore text"),
            Some(9)
        );

        // 没有双换行符的情况
        assert_eq!(find_real_thinking_end_tag("</thinking>"), None);
        assert_eq!(find_real_thinking_end_tag("</thinking>\n"), None);
        assert_eq!(find_real_thinking_end_tag("</thinking> more"), None);
    }

    #[test]
    fn test_find_real_thinking_end_tag_with_backticks() {
        // 被反引号包裹的应该被跳过
        assert_eq!(find_real_thinking_end_tag("`</thinking>`\n\n"), None);
        assert_eq!(
            find_real_thinking_end_tag("mention `</thinking>` in code\n\n"),
            None
        );

        // 只有前面有反引号
        assert_eq!(find_real_thinking_end_tag("`</thinking>\n\n"), None);

        // 只有后面有反引号
        assert_eq!(find_real_thinking_end_tag("</thinking>`\n\n"), None);
    }

    #[test]
    fn test_find_real_thinking_end_tag_with_quotes() {
        // 被双引号包裹的应该被跳过
        assert_eq!(find_real_thinking_end_tag("\"</thinking>\"\n\n"), None);
        assert_eq!(
            find_real_thinking_end_tag("the string \"</thinking>\" is a tag\n\n"),
            None
        );

        // 被单引号包裹的应该被跳过
        assert_eq!(find_real_thinking_end_tag("'</thinking>'\n\n"), None);
        assert_eq!(
            find_real_thinking_end_tag("use '</thinking>' as marker\n\n"),
            None
        );

        // 混合情况：双引号包裹后有真正的标签
        assert_eq!(
            find_real_thinking_end_tag("about \"</thinking>\" tag</thinking>\n\n"),
            Some(23)
        );

        // 混合情况：单引号包裹后有真正的标签
        assert_eq!(
            find_real_thinking_end_tag("about '</thinking>' tag</thinking>\n\n"),
            Some(23)
        );
    }

    #[test]
    fn test_find_real_thinking_end_tag_mixed() {
        // 先有被包裹的，后有真正的结束标签
        assert_eq!(
            find_real_thinking_end_tag("discussing `</thinking>` tag</thinking>\n\n"),
            Some(28)
        );

        // 多个被包裹的，最后一个是真正的
        assert_eq!(
            find_real_thinking_end_tag("`</thinking>` and `</thinking>` done</thinking>\n\n"),
            Some(36)
        );

        // 多种引用字符混合
        assert_eq!(
            find_real_thinking_end_tag(
                "`</thinking>` and \"</thinking>\" and '</thinking>' done</thinking>\n\n"
            ),
            Some(54)
        );
    }

    #[test]
    fn test_tool_use_immediately_after_thinking_filters_end_tag_and_closes_thinking_block() {
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();

        // thinking 内容以 `</thinking>` 结尾，但后面没有 `\n\n`（模拟紧跟 tool_use 的场景）
        all_events.extend(ctx.process_assistant_response("<thinking>abc</thinking>"));

        let tool_events = ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
            name: "Write".to_string(),
            tool_use_id: "tool_1".to_string(),
            input: "{}".to_string(),
            stop: false,
        });
        all_events.extend(tool_events);

        all_events.extend(ctx.generate_final_events());

        // 不应把 `</thinking>` 当作 thinking 内容输出
        assert!(
            all_events.iter().all(|e| {
                !(e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "thinking_delta"
                    && e.data["delta"]["thinking"] == "</thinking>")
            }),
            "`</thinking>` should be filtered from output"
        );

        // thinking block 必须在 tool_use block 之前关闭
        let thinking_index = ctx
            .thinking_block_index
            .expect("thinking block index should exist");
        let pos_thinking_stop = all_events.iter().position(|e| {
            e.event == "content_block_stop"
                && e.data["index"].as_i64() == Some(thinking_index as i64)
        });
        let pos_tool_start = all_events.iter().position(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
        });
        assert!(
            pos_thinking_stop.is_some(),
            "thinking block should be stopped"
        );
        assert!(pos_tool_start.is_some(), "tool_use block should be started");
        assert!(
            pos_thinking_stop.unwrap() < pos_tool_start.unwrap(),
            "thinking block should stop before tool_use block starts"
        );
    }

    #[test]
    fn test_final_flush_filters_standalone_thinking_end_tag() {
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>abc</thinking>"));
        all_events.extend(ctx.generate_final_events());

        assert!(
            all_events.iter().all(|e| {
                !(e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "thinking_delta"
                    && e.data["delta"]["thinking"] == "</thinking>")
            }),
            "`</thinking>` should be filtered during final flush"
        );
    }

    #[test]
    fn test_thinking_strips_leading_newline_same_chunk() {
        // <thinking>\n 在同一个 chunk 中，\n 应被剥离
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_assistant_response("<thinking>\nHello world");

        // 找到所有 thinking_delta 事件
        let thinking_deltas: Vec<_> = events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .collect();

        // 拼接所有 thinking 内容
        let full_thinking: String = thinking_deltas
            .iter()
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !full_thinking.starts_with('\n'),
            "thinking content should not start with \\n, got: {:?}",
            full_thinking
        );
    }

    #[test]
    fn test_thinking_strips_leading_newline_cross_chunk() {
        // <thinking> 在第一个 chunk 末尾，\n 在第二个 chunk 开头
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let events1 = ctx.process_assistant_response("<thinking>");
        let events2 = ctx.process_assistant_response("\nHello world");

        let mut all_events = Vec::new();
        all_events.extend(events1);
        all_events.extend(events2);

        let thinking_deltas: Vec<_> = all_events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .collect();

        let full_thinking: String = thinking_deltas
            .iter()
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !full_thinking.starts_with('\n'),
            "thinking content should not start with \\n across chunks, got: {:?}",
            full_thinking
        );
    }

    #[test]
    fn test_thinking_no_strip_when_no_leading_newline() {
        // <thinking> 后直接跟内容（无 \n），内容应完整保留
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_assistant_response("<thinking>abc</thinking>\n\ntext");

        let thinking_deltas: Vec<_> = events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .collect();

        let full_thinking: String = thinking_deltas
            .iter()
            .filter(|e| {
                !e.data["delta"]["thinking"]
                    .as_str()
                    .unwrap_or("")
                    .is_empty()
            })
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .collect();

        assert_eq!(full_thinking, "abc", "thinking content should be 'abc'");
    }

    #[test]
    fn test_text_after_thinking_strips_leading_newlines() {
        // `</thinking>\n\n` 后的文本不应以 \n\n 开头
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_assistant_response("<thinking>\nabc</thinking>\n\n你好");

        let text_deltas: Vec<_> = events
            .iter()
            .filter(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "text_delta")
            .collect();

        let full_text: String = text_deltas
            .iter()
            .map(|e| e.data["delta"]["text"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !full_text.starts_with('\n'),
            "text after thinking should not start with \\n, got: {:?}",
            full_text
        );
        assert_eq!(full_text, "你好");
    }

    /// 辅助函数：从事件列表中提取所有 thinking_delta 的拼接内容
    fn collect_thinking_content(events: &[SseEvent]) -> String {
        events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// 辅助函数：从事件列表中提取所有 text_delta 的拼接内容
    fn collect_text_content(events: &[SseEvent]) -> String {
        events
            .iter()
            .filter(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "text_delta")
            .map(|e| e.data["delta"]["text"].as_str().unwrap_or(""))
            .collect()
    }

    fn invoke_test_tools() -> HashSet<String> {
        [
            "exec_command",
            "write_file",
            "apply_patch",
            "tool_a",
            "tool_b",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn collect_tool_uses(events: &[SseEvent]) -> Vec<(String, String)> {
        let mut result = Vec::new();
        for start in events.iter().filter(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
        }) {
            let index = start.data["index"].as_i64();
            let name = start.data["content_block"]["name"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let input = events
                .iter()
                .filter(|e| {
                    e.event == "content_block_delta"
                        && e.data["index"].as_i64() == index
                        && e.data["delta"]["type"] == "input_json_delta"
                })
                .map(|e| e.data["delta"]["partial_json"].as_str().unwrap_or(""))
                .collect::<String>();
            result.push((name, input));
        }
        result
    }

    #[test]
    fn test_stream_recovers_single_bare_literal_invoke() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "<invoke name=\"exec_command\"><parameter name=\"cmd\">pwd</parameter></invoke>",
        ));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(tools.len(), 1, "bare invoke should be recovered: {tools:?}");
        assert_eq!(tools[0].0, "exec_command");
        let parsed: serde_json::Value = serde_json::from_str(&tools[0].1).unwrap();
        assert_eq!(parsed["cmd"], "pwd");
        assert!(!collect_text_content(&all).contains("<invoke"));
    }

    #[test]
    fn test_stream_recovers_split_literal_invoke() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("准备执行。\ncount\n<function_calls>\n<inv"));
        all.extend(ctx.process_assistant_response("oke name=\"exec_command\">"));
        all.extend(ctx.process_assistant_response(
            "<parameter name=\"cmd\">ls</parameter></invoke>\n</function_calls>",
        ));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(
            tools.len(),
            1,
            "literal invoke should become one tool_use: {tools:?}"
        );
        assert_eq!(tools[0].0, "exec_command");
        let parsed: serde_json::Value = serde_json::from_str(&tools[0].1).unwrap();
        assert_eq!(parsed["cmd"], "ls");

        let text = collect_text_content(&all);
        assert!(text.contains("准备执行。"));
        assert!(!text.contains("<invoke"));
        assert!(!text.contains("function_calls"));
        assert!(!text.contains("count\n<"));
    }

    #[test]
    fn test_stream_recovers_sequential_literal_invokes() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "<invoke name=\"tool_a\"><parameter name=\"x\">1</parameter></invoke><invoke name=\"tool_b\"><parameter name=\"y\">2</parameter></invoke>",
        ));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(
            tools.len(),
            2,
            "sequential invokes should become two tool_uses: {tools:?}"
        );
        assert_eq!(tools[0].0, "tool_a");
        assert_eq!(tools[1].0, "tool_b");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tools[0].1).unwrap()["x"],
            1
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tools[1].1).unwrap()["y"],
            2
        );
    }

    #[test]
    fn test_stream_recovers_large_multiline_literal_invoke_split_by_lines() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let patch = (0..48)
            .map(|i| format!("+ patch line {i:02} keeps multiline body intact"))
            .collect::<Vec<_>>()
            .join("\n");
        let full = format!(
            "<invoke name=\"apply_patch\"><parameter name=\"patch\">{}</parameter></invoke>",
            patch
        );

        let mut all = Vec::new();
        for line in full.split_inclusive('\n') {
            all.extend(ctx.process_assistant_response(line));
        }
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(
            tools.len(),
            1,
            "multiline split invoke should remain held until closed: {tools:?}"
        );
        assert_eq!(tools[0].0, "apply_patch");
        let parsed: serde_json::Value = serde_json::from_str(&tools[0].1).unwrap();
        assert_eq!(parsed["patch"], patch);
    }

    #[test]
    fn test_stream_recovers_parameter_values_containing_literal_close_tags() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let patch =
            "*** Begin Patch\n- keep literal </parameter>\n+ keep literal </invoke>\n*** End Patch";
        let payload = format!(
            "<invoke name=\"apply_patch\"><parameter name=\"patch\">{}</parameter></invoke>",
            patch
        );

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(&payload));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(
            tools.len(),
            1,
            "literal close tags inside values should not split"
        );
        let parsed: serde_json::Value = serde_json::from_str(&tools[0].1).unwrap();
        assert_eq!(parsed["patch"], patch);
    }

    #[test]
    fn test_stream_dedupes_leaked_invoke_against_later_structured_tool_use() {
        use crate::kiro::model::events::ToolUseEvent;

        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "count\n<invoke name=\"exec_command\"><parameter name=\"cmd\">ls</parameter></invoke>",
        ));
        all.extend(ctx.process_tool_use(&ToolUseEvent {
            name: "exec_command".to_string(),
            tool_use_id: "toolu_structured".to_string(),
            input: r#"{"cmd":"ls"}"#.to_string(),
            stop: true,
        }));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(
            tools.len(),
            1,
            "leaked and structured duplicates must collapse: {tools:?}"
        );
        assert_eq!(tools[0].0, "exec_command");
    }

    #[test]
    fn test_stream_maps_short_leaked_tool_name_back_to_original() {
        let original = "mcp__very_long_server_name__tool_with_name_that_exceeds_kiro_limit";
        let short = "mcpVeryLongServerNameToolWithNameThatExceedsKiroLiHash12345678";
        let mut map = HashMap::new();
        map.insert(short.to_string(), original.to_string());
        let known = [short.to_string(), original.to_string()]
            .into_iter()
            .collect::<HashSet<_>>();

        let mut ctx =
            StreamContext::new_with_thinking_with_known_tools("test-model", 1, false, map, known);
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(&format!(
            "count\n<invoke name=\"{}\"><parameter name=\"query\">needle</parameter></invoke>",
            short
        )));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, original);
        let parsed: serde_json::Value = serde_json::from_str(&tools[0].1).unwrap();
        assert_eq!(parsed["query"], "needle");
    }

    #[test]
    fn test_stream_does_not_recover_unknown_or_fenced_invoke() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "```\n<invoke name=\"exec_command\"><parameter name=\"cmd\">rm</parameter></invoke>\n```\n",
        ));
        all.extend(ctx.process_assistant_response(
            "count\n<invoke name=\"unknown_tool\"><parameter name=\"x\">1</parameter></invoke>",
        ));
        all.extend(ctx.generate_final_events());

        let tools = collect_tool_uses(&all);
        assert!(
            tools.is_empty(),
            "fenced/unknown invoke must remain text: {tools:?}"
        );
        let text = collect_text_content(&all);
        assert!(text.contains("<invoke name=\"exec_command\">"));
        assert!(text.contains("unknown_tool"));
    }

    #[test]
    fn test_stream_does_not_recover_with_empty_known_tool_table() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            HashSet::new(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "count\n<invoke name=\"exec_command\"><parameter name=\"cmd\">rm</parameter></invoke>",
        ));
        all.extend(ctx.generate_final_events());

        assert!(collect_tool_uses(&all).is_empty());
        let text = collect_text_content(&all);
        assert!(text.contains("<invoke name=\"exec_command\">"));
        assert!(text.contains("rm"));
    }

    #[test]
    fn test_stream_does_not_recover_markdown_list_or_blockquote_invokes() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "- <invoke name=\"exec_command\"><parameter name=\"cmd\">rm -rf /</parameter></invoke>\n",
        ));
        all.extend(ctx.process_assistant_response(
            "> <invoke name=\"exec_command\"><parameter name=\"cmd\">rm -rf /tmp</parameter></invoke>",
        ));
        all.extend(ctx.generate_final_events());

        assert!(collect_tool_uses(&all).is_empty());
        let text = collect_text_content(&all);
        assert!(text.contains("rm -rf /"));
        assert!(text.contains("rm -rf /tmp"));
    }

    #[test]
    fn test_stream_flushes_unclosed_line_start_invoke_as_text() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response(
            "<invoke name=\"exec_command\"><parameter name=\"cmd\">ls",
        ));
        all.extend(ctx.generate_final_events());

        assert!(collect_tool_uses(&all).is_empty());
        let text = collect_text_content(&all);
        assert!(text.contains("<invoke name=\"exec_command\">"));
        assert!(text.contains("ls"));
    }

    #[test]
    fn test_stream_stray_token_repeat_guard_drops_flood_tail() {
        let mut ctx = StreamContext::new_with_thinking_with_known_tools(
            "test-model",
            1,
            false,
            HashMap::new(),
            invoke_test_tools(),
        );
        let _ = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(
            ctx.process_assistant_response(&format!("{}SHOULD_NOT_APPEAR", "count\n".repeat(40))),
        );
        all.extend(ctx.generate_final_events());

        let text = collect_text_content(&all);
        assert!(
            !text.contains("SHOULD_NOT_APPEAR"),
            "repeat guard should drop text after a stray-token flood: {text:?}"
        );
        assert!(
            text.lines().filter(|line| line.trim() == "count").count()
                < REPEAT_GUARD_TRIP_THRESHOLD as usize,
            "guard should stop before emitting the threshold line: {text:?}"
        );
    }

    #[test]
    fn test_extract_invoke_content_blocks_restores_typed_params() {
        let blocks = extract_invoke_content_blocks(
            "count\n<invoke name=\"exec_command\"><parameter name=\"flag\">true</parameter><parameter name=\"n\">42</parameter><parameter name=\"cmd\">echo hi</parameter></invoke>",
            &invoke_test_tools(),
            &HashMap::new(),
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["input"]["flag"], true);
        assert_eq!(blocks[0]["input"]["n"], 42);
        assert_eq!(blocks[0]["input"]["cmd"], "echo hi");
    }

    #[test]
    fn test_extract_invoke_content_blocks_maps_short_name_and_keeps_text_order() {
        let mut map = HashMap::new();
        map.insert(
            "shortTool".to_string(),
            "mcp__server__short_tool".to_string(),
        );
        let known = ["shortTool".to_string()]
            .into_iter()
            .collect::<HashSet<_>>();
        let blocks = extract_invoke_content_blocks(
            "before\ncall\n<function_calls>\n<invoke name=\"shortTool\"><parameter name=\"x\">y</parameter></invoke>\n</function_calls>\nafter",
            &known,
            &map,
        );

        assert_eq!(
            blocks.len(),
            3,
            "text/tool/text blocks expected: {blocks:?}"
        );
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "before\n");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["name"], "mcp__server__short_tool");
        assert_eq!(blocks[1]["input"]["x"], "y");
        assert_eq!(blocks[2]["type"], "text");
        assert_eq!(blocks[2]["text"], "after");
    }

    #[test]
    fn test_end_tag_newlines_split_across_events() {
        // `</thinking>\n` 在 chunk 1，`\n` 在 chunk 2，`text` 在 chunk 3
        // 确保 `</thinking>` 不会被部分当作 thinking 内容发出
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>\n"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("你好"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "abc",
            "thinking should be 'abc', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "你好", "text should be '你好', got: {:?}", text);
    }

    #[test]
    fn test_end_tag_alone_in_chunk_then_newlines_in_next() {
        // `</thinking>` 单独在一个 chunk，`\n\ntext` 在下一个 chunk
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>"));
        all.extend(ctx.process_assistant_response("\n\n你好"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "abc",
            "thinking should be 'abc', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "你好", "text should be '你好', got: {:?}", text);
    }

    #[test]
    fn test_start_tag_newline_split_across_events() {
        // `\n\n` 在 chunk 1，`<thinking>` 在 chunk 2，`\n` 在 chunk 3
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("\n\n"));
        all.extend(ctx.process_assistant_response("<thinking>"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("abc</thinking>\n\ntext"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "abc",
            "thinking should be 'abc', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "text", "text should be 'text', got: {:?}", text);
    }

    #[test]
    fn test_full_flow_maximally_split() {
        // 极端拆分：每个关键边界都在不同 chunk
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        // \n\n<thinking>\n 拆成多段
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("<thin"));
        all.extend(ctx.process_assistant_response("king>"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("hello"));
        // </thinking>\n\n 拆成多段
        all.extend(ctx.process_assistant_response("</thi"));
        all.extend(ctx.process_assistant_response("nking>"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("world"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "hello",
            "thinking should be 'hello', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "world", "text should be 'world', got: {:?}", text);
    }

    #[test]
    fn test_thinking_only_sets_max_tokens_stop_reason() {
        // 整个流只有 thinking 块，没有 text 也没有 tool_use，stop_reason 应为 max_tokens
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>"));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("should have message_delta event");

        assert_eq!(
            message_delta.data["delta"]["stop_reason"], "max_tokens",
            "stop_reason should be max_tokens when only thinking is produced"
        );

        // 应补发一套完整的 text 事件（content_block_start + delta 空格 + content_block_stop）
        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_start" && e.data["content_block"]["type"] == "text"
            }),
            "should emit text content_block_start"
        );
        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"] == " "
            }),
            "should emit text_delta with a single space"
        );
        // text block 应被 generate_final_events 自动关闭
        let text_block_index = all_events
            .iter()
            .find_map(|e| {
                if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                    e.data["index"].as_i64()
                } else {
                    None
                }
            })
            .expect("text block should exist");
        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_stop"
                    && e.data["index"].as_i64() == Some(text_block_index)
            }),
            "text block should be stopped"
        );
    }

    #[test]
    fn test_thinking_with_text_keeps_end_turn_stop_reason() {
        // thinking + text 的情况，stop_reason 应为 end_turn
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>\n\nHello"));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("should have message_delta event");

        assert_eq!(
            message_delta.data["delta"]["stop_reason"], "end_turn",
            "stop_reason should be end_turn when text is also produced"
        );
    }

    #[test]
    fn test_thinking_with_tool_use_keeps_tool_use_stop_reason() {
        // thinking + tool_use 的情况，stop_reason 应为 tool_use
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>"));
        all_events.extend(
            ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
                name: "test_tool".to_string(),
                tool_use_id: "tool_1".to_string(),
                input: "{}".to_string(),
                stop: true,
            }),
        );
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("should have message_delta event");

        assert_eq!(
            message_delta.data["delta"]["stop_reason"], "tool_use",
            "stop_reason should be tool_use when tool_use is present"
        );
    }
}
