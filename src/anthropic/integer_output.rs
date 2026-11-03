//! Request-scoped guidance for prompts that require an exact integer-list size.
//!
//! This stays at request-body preparation so ordinary messages, routing, and response protocol
//! handling keep their existing semantics.

use bytes::Bytes;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

use super::types::{MessagesRequest, SystemMessage};

const MARKER: &str = r#"<integer_output_constraint version="v1">"#;
const END_MARKER: &str = "</integer_output_constraint>";
const MAX_COUNT: usize = 4_096;

static CHINESE_INTEGER_LIST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?P<count>[1-9]\d{0,4})\s*个\s*(?P<min>\d{1,9})\s*(?:到|至|[-－])\s*(?P<max>\d{1,9})\s*(?:（\s*含端点\s*）|\(\s*含端点\s*\))?\s*(?:的\s*)?整数",
    )
    .expect("integer-list constraint regex must compile")
});

static ENGLISH_INTEGER_LIST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?P<count>[1-9]\d{0,4})\s+(?:integers|numbers)\b.*?\b(?:between|from)\s+(?P<min>\d{1,9})\s+(?:and|to)\s+(?P<max>\d{1,9})\b",
    )
    .expect("English integer-list constraint regex must compile")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntegerOutputConstraint {
    pub(crate) count: usize,
    pub(crate) min: u64,
    pub(crate) max: u64,
}

impl IntegerOutputConstraint {
    fn from_match(captures: &regex::Captures<'_>) -> Option<Self> {
        let count = captures.name("count")?.as_str().parse().ok()?;
        let min = captures.name("min")?.as_str().parse().ok()?;
        let max = captures.name("max")?.as_str().parse().ok()?;
        (count <= MAX_COUNT && min <= max).then_some(Self { count, min, max })
    }

    fn instruction(self) -> String {
        format!(
            "{MARKER}\nOutput exactly {} integer values in the inclusive range {} through {}. Output only those values, separated by commas. Do not add labels, prose, code fences, indexes, or any other numbers. Stop immediately after the {}th value.\n{END_MARKER}",
            self.count, self.min, self.max, self.count
        )
    }
}

pub(crate) fn from_request_body(body: &[u8]) -> Option<IntegerOutputConstraint> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    from_value(&value)
}

pub(crate) fn from_messages_request(request: &MessagesRequest) -> Option<IntegerOutputConstraint> {
    let value = serde_json::to_value(request).ok()?;
    from_value(&value)
}

fn from_value(value: &Value) -> Option<IntegerOutputConstraint> {
    let messages = value.get("messages")?.as_array()?;
    let message = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))?;
    let text = user_message_text(message)?;
    CHINESE_INTEGER_LIST
        .captures(&text)
        .and_then(|captures| IntegerOutputConstraint::from_match(&captures))
        .or_else(|| {
            ENGLISH_INTEGER_LIST
                .captures(&text)
                .and_then(|captures| IntegerOutputConstraint::from_match(&captures))
        })
}

pub(crate) fn append_to_messages_request(
    request: &mut MessagesRequest,
    constraint: IntegerOutputConstraint,
) -> bool {
    if request
        .system
        .as_ref()
        .is_some_and(|blocks| blocks.iter().any(|block| block.text.contains(MARKER)))
    {
        return false;
    }
    request
        .system
        .get_or_insert_with(Vec::new)
        .push(SystemMessage {
            text: constraint.instruction(),
            cache_control: None,
        });
    true
}

pub(crate) fn rewrite_body(body: &Bytes) -> Bytes {
    let Some(constraint) = from_request_body(body) else {
        return body.clone();
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return body.clone();
    };
    if append_constraint(&mut value, constraint) {
        serde_json::to_vec(&value)
            .map(Bytes::from)
            .unwrap_or_else(|_| body.clone())
    } else {
        body.clone()
    }
}

pub(crate) fn append_constraint(value: &mut Value, constraint: IntegerOutputConstraint) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    if object
        .get("system")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(MARKER))
            })
        })
    {
        return false;
    }

    let block = json!({"type": "text", "text": constraint.instruction()});
    match object.get_mut("system") {
        None | Some(Value::Null) => {
            object.insert("system".to_string(), Value::Array(vec![block]));
        }
        Some(Value::String(existing)) => {
            let existing = std::mem::take(existing);
            *object.get_mut("system").expect("system field exists") =
                Value::Array(vec![json!({"type": "text", "text": existing}), block]);
        }
        Some(Value::Array(blocks)) => blocks.push(block),
        Some(_) => return false,
    }
    true
}

fn user_message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(prompt: Value, system: Option<Value>) -> Bytes {
        let mut value = json!({
            "model": "claude-opus-5-5",
            "max_tokens": 16384,
            "stream": true,
            "messages": [{"role": "user", "content": prompt}]
        });
        if let Some(system) = system {
            value["system"] = system;
        }
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn detects_chinese_exact_integer_list_request() {
        let body = body(json!("分别选择 300 个 1 到 355（含端点）的整数。"), None);
        assert_eq!(
            from_request_body(&body),
            Some(IntegerOutputConstraint {
                count: 300,
                min: 1,
                max: 355
            })
        );
    }

    #[test]
    fn detects_english_exact_integer_list_request() {
        let body = body(json!("Return 12 integers between 1 and 20."), None);
        assert_eq!(
            from_request_body(&body),
            Some(IntegerOutputConstraint {
                count: 12,
                min: 1,
                max: 20
            })
        );
    }

    #[test]
    fn rewrites_only_matching_requests_and_preserves_existing_system() {
        let matching_body = body(
            json!("给出 3 个 1 到 5 的整数。"),
            Some(json!("client system")),
        );
        let rewritten = rewrite_body(&matching_body);
        let value: Value = serde_json::from_slice(&rewritten).unwrap();
        let system = value["system"].as_array().unwrap();
        assert_eq!(system[0]["text"], "client system");
        assert!(system[1]["text"].as_str().unwrap().contains(MARKER));
        assert_eq!(rewrite_body(&rewritten), rewritten);

        let ordinary = body(json!("Give me a short explanation."), None);
        assert_eq!(rewrite_body(&ordinary), ordinary);
    }

    #[test]
    fn ignores_invalid_ranges_and_non_user_text() {
        let invalid = body(json!("给出 3 个 9 到 1 的整数。"), None);
        assert!(from_request_body(&invalid).is_none());
        let tool_result = json!([{
            "type": "tool_result",
            "content": "给出 3 个 1 到 5 的整数。"
        }]);
        let tool_result_body = body(tool_result, None);
        assert!(from_request_body(&tool_result_body).is_none());
    }
}
