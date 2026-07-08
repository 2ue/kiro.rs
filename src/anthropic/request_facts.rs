//! Lightweight facts extracted from Anthropic Messages request bodies.
//!
//! This module is intentionally narrower than full request parsing. Raw external
//! passthrough paths use it to inspect or patch top-level fields without
//! deserializing `messages`, images, tool results, or other heavy body content.

use bytes::Bytes;

#[derive(Debug, Default, Clone)]
pub(crate) struct RawMessagesBodyProbe {
    pub(crate) model: Option<String>,
    pub(crate) stream: Option<bool>,
    pub(crate) max_tokens_present: bool,
    pub(crate) complete_top_level_object: bool,
    object_start_index: Option<usize>,
    model_value_span: Option<std::ops::Range<usize>>,
    object_end_index: Option<usize>,
}

pub(crate) fn probe_raw_messages_body(raw_body: &Bytes) -> RawMessagesBodyProbe {
    scan_raw_top_level_messages_body(raw_body.as_ref())
}

pub(crate) fn raw_messages_body_hints(raw_body: &Bytes) -> (Option<String>, Option<bool>) {
    let probe = probe_raw_messages_body(raw_body);
    (probe.model, probe.stream)
}

pub(crate) fn rewrite_raw_missing_top_level_max_tokens_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
    default_value: i32,
) -> Result<Option<Bytes>, String> {
    if probe.max_tokens_present {
        return Ok(None);
    }
    if !probe.complete_top_level_object {
        return Ok(None);
    }
    let Some(object_start) = probe.object_start_index else {
        return Ok(None);
    };
    let Some(object_end) = probe.object_end_index else {
        return Ok(None);
    };
    let has_existing_fields = raw_body[object_start + 1..object_end]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace());
    let field = if has_existing_fields {
        format!(r#","max_tokens":{}"#, default_value)
    } else {
        format!(r#""max_tokens":{}"#, default_value)
    };
    let mut out = Vec::with_capacity(raw_body.len().saturating_add(field.len()));
    out.extend_from_slice(&raw_body[..object_end]);
    out.extend_from_slice(field.as_bytes());
    out.extend_from_slice(&raw_body[object_end..]);
    Ok(Some(Bytes::from(out)))
}

pub(crate) fn rewrite_raw_top_level_model(raw_body: &Bytes, model: &str) -> Result<Bytes, String> {
    let probe = probe_raw_messages_body(raw_body);
    let Some(span) = probe.model_value_span else {
        return Err("top-level model field was not found".to_string());
    };
    let encoded_model = serde_json::to_string(model).map_err(|err| err.to_string())?;
    let mut out = Vec::with_capacity(
        raw_body
            .len()
            .saturating_sub(span.end.saturating_sub(span.start))
            .saturating_add(encoded_model.len()),
    );
    out.extend_from_slice(&raw_body[..span.start]);
    out.extend_from_slice(encoded_model.as_bytes());
    out.extend_from_slice(&raw_body[span.end..]);
    Ok(Bytes::from(out))
}

fn scan_raw_top_level_messages_body(bytes: &[u8]) -> RawMessagesBodyProbe {
    let mut probe = RawMessagesBodyProbe::default();
    let mut i = skip_json_ws(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return probe;
    }
    probe.object_start_index = Some(i);
    i += 1;

    loop {
        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b'}') => {
                let doc_end = skip_json_ws(bytes, i + 1);
                if doc_end == bytes.len() {
                    probe.complete_top_level_object = true;
                    probe.object_end_index = Some(i);
                }
                return probe;
            }
            None => return probe,
            Some(b'"') => {}
            _ => return probe,
        }

        let Some((key, key_end)) = parse_json_string_at(bytes, i) else {
            return probe;
        };
        i = skip_json_ws(bytes, key_end);
        if bytes.get(i) != Some(&b':') {
            return probe;
        }
        i = skip_json_ws(bytes, i + 1);
        let value_start = i;

        if key == "model" {
            if let Some((model, value_end)) = parse_json_string_at(bytes, value_start) {
                probe.model = Some(model);
                probe.model_value_span = Some(value_start..value_end);
                i = value_end;
            } else {
                let Some(value_end) = skip_json_value(bytes, value_start) else {
                    return probe;
                };
                i = value_end;
            }
        } else if key == "stream" {
            if bytes.get(value_start..value_start.saturating_add(4)) == Some(b"true") {
                probe.stream = Some(true);
                i = value_start + 4;
            } else if bytes.get(value_start..value_start.saturating_add(5)) == Some(b"false") {
                probe.stream = Some(false);
                i = value_start + 5;
            } else {
                let Some(value_end) = skip_json_value(bytes, value_start) else {
                    return probe;
                };
                i = value_end;
            }
        } else if key == "max_tokens" {
            probe.max_tokens_present = true;
            let Some(value_end) = skip_json_value(bytes, value_start) else {
                return probe;
            };
            i = value_end;
        } else {
            let Some(value_end) = skip_json_value(bytes, value_start) else {
                return probe;
            };
            i = value_end;
        }

        i = skip_json_ws(bytes, i);
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => {
                let doc_end = skip_json_ws(bytes, i + 1);
                if doc_end == bytes.len() {
                    probe.complete_top_level_object = true;
                    probe.object_end_index = Some(i);
                }
                return probe;
            }
            None => return probe,
            _ => return probe,
        }
    }
}

fn skip_json_ws(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(|byte| byte.is_ascii_whitespace()) {
        i += 1;
    }
    i
}

fn parse_json_string_at(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let end = skip_json_string(bytes, start)?;
    let value = serde_json::from_slice::<String>(&bytes[start..end]).ok()?;
    Some((value, end))
}

fn skip_json_string(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut i = start + 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(i).copied() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn skip_json_value(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = skip_json_ws(bytes, start);
    match bytes.get(i).copied()? {
        b'"' => skip_json_string(bytes, i),
        b'{' | b'[' => skip_json_container(bytes, i),
        b't' if bytes.get(i..i + 4) == Some(b"true") => Some(i + 4),
        b'f' if bytes.get(i..i + 5) == Some(b"false") => Some(i + 5),
        b'n' if bytes.get(i..i + 4) == Some(b"null") => Some(i + 4),
        b'-' | b'0'..=b'9' => {
            while bytes.get(i).is_some_and(|byte| {
                !matches!(byte, b',' | b'}' | b']') && !byte.is_ascii_whitespace()
            }) {
                i += 1;
            }
            Some(i)
        }
        _ => None,
    }
}

fn skip_json_container(bytes: &[u8], start: usize) -> Option<usize> {
    let first = bytes.get(start).copied()?;
    if !matches!(first, b'{' | b'[') {
        return None;
    }
    let mut stack = vec![first];
    let mut i = start + 1;
    while let Some(byte) = bytes.get(i).copied() {
        match byte {
            b'"' => {
                i = skip_json_string(bytes, i)?;
                continue;
            }
            b'{' | b'[' => stack.push(byte),
            b'}' => {
                if stack.pop() != Some(b'{') {
                    return None;
                }
                if stack.is_empty() {
                    return Some(i + 1);
                }
            }
            b']' => {
                if stack.pop() != Some(b'[') {
                    return None;
                }
                if stack.is_empty() {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_hints_ignore_nested_model_without_top_level_model() {
        let raw = Bytes::from(
            json!({
                "messages": [
                    {
                        "role": "user",
                        "content": [{"type": "text", "model": "nested"}]
                    }
                ],
                "stream": true
            })
            .to_string(),
        );

        let (model, stream) = raw_messages_body_hints(&raw);

        assert_eq!(model, None);
        assert_eq!(stream, Some(true));
    }

    #[test]
    fn raw_top_level_model_rewrite_preserves_nested_content() {
        let raw = Bytes::from_static(
            br#"{"messages":[{"role":"user","content":[{"type":"text","text":"model old"}]}],"model":"old","stream":false}"#,
        );

        let rewritten = rewrite_raw_top_level_model(&raw, "new").expect("rewrite");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");

        assert_eq!(value["model"], "new");
        assert_eq!(value["messages"][0]["content"][0]["text"], "model old");
        assert_eq!(value["stream"], false);
    }

    #[test]
    fn raw_probe_detects_missing_max_tokens_and_rewrite_inserts_field() {
        let raw = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        );

        let probe = probe_raw_messages_body(&raw);
        assert_eq!(probe.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(probe.stream, Some(true));
        assert!(!probe.max_tokens_present);
        assert!(probe.complete_top_level_object);

        let rewritten = rewrite_raw_missing_top_level_max_tokens_with_probe(&raw, &probe, 4096)
            .expect("rewrite")
            .expect("missing max tokens");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");
        assert_eq!(value["max_tokens"], 4096);
        assert_eq!(value["messages"][0]["content"], "hi");
    }

    #[test]
    fn raw_probe_does_not_rewrite_when_max_tokens_exists_or_json_incomplete() {
        let raw = Bytes::from_static(br#"{"model":"m","max_tokens":16,"messages":[]}"#);
        let probe = probe_raw_messages_body(&raw);
        assert!(
            rewrite_raw_missing_top_level_max_tokens_with_probe(&raw, &probe, 4096)
                .expect("probe")
                .is_none()
        );

        let incomplete = Bytes::from_static(br#"{"model":"m","messages":[]"#);
        let probe = probe_raw_messages_body(&incomplete);
        assert!(!probe.complete_top_level_object);
        assert!(
            rewrite_raw_missing_top_level_max_tokens_with_probe(&incomplete, &probe, 4096)
                .expect("probe")
                .is_none()
        );
    }

    #[test]
    fn raw_missing_max_tokens_rewrite_handles_whitespace_around_object() {
        let raw = Bytes::from_static(br#"  { "model":"m","messages":[] }  "#);
        let probe = probe_raw_messages_body(&raw);

        let rewritten = rewrite_raw_missing_top_level_max_tokens_with_probe(&raw, &probe, 4096)
            .expect("rewrite")
            .expect("missing max tokens");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");

        assert_eq!(value["max_tokens"], 4096);
        assert_eq!(value["model"], "m");
    }
}
