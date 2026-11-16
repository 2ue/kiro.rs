use super::prompt_cache::canonicalize_cache_value;
use super::types::MessagesRequest;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) fn extract_stable_conversation_id(req: &MessagesRequest) -> Option<String> {
    extract_metadata_conversation_id(req).or_else(|| derive_fallback_conversation_id(req))
}

pub(crate) fn extract_metadata_conversation_id(req: &MessagesRequest) -> Option<String> {
    req.metadata
        .as_ref()
        .and_then(|m| m.user_id.as_ref())
        .and_then(|user_id| extract_session_id(user_id))
}

fn extract_session_id(user_id: &str) -> Option<String> {
    // 先尝试 JSON 解析
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(user_id) {
        if let Some(session_id) = json.get("session_id").and_then(|v| v.as_str()) {
            if is_valid_uuid(session_id) {
                return Some(session_id.to_string());
            }
        }
    }

    // 回退到字符串格式: 查找 "session_" 后面的内容
    if let Some(pos) = user_id.find("session_") {
        let session_part = &user_id[pos + 8..]; // "session_" 长度为 8
        // 按字节截 36 个字符：`user_id` 来自客户端，截断点可能落在多字节字符中间，
        // 直接切片会 panic 并让这个请求的连接直接断掉。`get` 在那种情况下返回 None。
        if let Some(uuid_str) = session_part.get(..36).filter(|s| is_valid_uuid(s)) {
            return Some(uuid_str.to_string());
        }
    }
    None
}

fn derive_fallback_conversation_id(req: &MessagesRequest) -> Option<String> {
    let seed = if let Some(first_user_message) =
        req.messages.iter().find(|message| message.role == "user")
    {
        serde_json::json!({
            "system": &req.system,
            "tools": &req.tools,
            "first_user_message": first_user_message,
        })
    } else {
        serde_json::json!({
            "system": &req.system,
            "tools": &req.tools,
            "messages": &req.messages,
        })
    };

    Some(deterministic_conversation_id(&canonicalize_cache_value(
        &seed,
    )))
}

fn deterministic_conversation_id(seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kiro.rs:anthropic:conversation-id:v1:");
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn is_valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}
