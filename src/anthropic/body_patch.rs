use bytes::Bytes;
use serde_json::Value;

use super::request_facts::{probe_raw_messages_body, rewrite_raw_top_level_model_with_probe};

#[derive(Debug, Clone)]
pub enum PatchOp {
    Set(Value),
    Remove,
}

#[derive(Debug, Clone)]
pub struct BodyPatch {
    pub pointer: String,
    pub op: PatchOp,
    pub source: &'static str,
}

pub const ALLOWED_POINTER_PREFIXES: &[&str] = &[
    "/model",
    "/max_tokens",
    "/thinking",
    "/output_config",
    "/system",
    "/messages",
    "/tools",
    "/context_management",
    "/metadata",
];

#[derive(Debug, Default)]
pub struct BodyPatchSet(Vec<BodyPatch>);

impl BodyPatchSet {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push(&mut self, patch: BodyPatch) {
        let allowed = ALLOWED_POINTER_PREFIXES.iter().any(|prefix| {
            patch.pointer == *prefix
                || patch
                    .pointer
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('/'))
        });
        if !allowed {
            tracing::error!(pointer = %patch.pointer, source = patch.source, "body patch outside allowlist");
            debug_assert!(allowed, "body patch outside allowlist: {}", patch.pointer);
            return;
        }
        self.0.push(patch);
    }

    pub fn set(&mut self, pointer: impl Into<String>, value: Value, source: &'static str) {
        self.push(BodyPatch {
            pointer: pointer.into(),
            op: PatchOp::Set(value),
            source,
        });
    }

    pub fn remove(&mut self, pointer: impl Into<String>, source: &'static str) {
        self.push(BodyPatch {
            pointer: pointer.into(),
            op: PatchOp::Remove,
            source,
        });
    }

    pub fn apply(&self, raw: &Bytes) -> Bytes {
        if self.is_empty() {
            return raw.clone();
        }
        if let [
            BodyPatch {
                pointer,
                op: PatchOp::Set(Value::String(model)),
                ..
            },
        ] = self.0.as_slice()
            && pointer == "/model"
        {
            let probe = probe_raw_messages_body(raw);
            if let Ok(body) = rewrite_raw_top_level_model_with_probe(raw, &probe, model) {
                return body;
            }
        }
        let Ok(mut value) = serde_json::from_slice::<Value>(raw) else {
            tracing::warn!("body patch JSON parse failed; preserving original bytes");
            return raw.clone();
        };
        for patch in &self.0 {
            tracing::trace!(pointer = %patch.pointer, source = patch.source, "applying route body patch");
            let Some((parent, key)) = patch.pointer.rsplit_once('/') else {
                continue;
            };
            let key = key.replace("~1", "/").replace("~0", "~");
            let Some(target) = value.pointer_mut(parent) else {
                continue;
            };
            match (target, &patch.op) {
                (Value::Object(object), PatchOp::Set(new)) => {
                    object.insert(key, new.clone());
                }
                (Value::Object(object), PatchOp::Remove) => {
                    object.shift_remove(&key);
                }
                (Value::Array(array), op) => {
                    if let Ok(index) = key.parse::<usize>() {
                        match op {
                            PatchOp::Set(new) if index < array.len() => array[index] = new.clone(),
                            PatchOp::Set(new) if index == array.len() => array.push(new.clone()),
                            PatchOp::Remove if index < array.len() => {
                                array.remove(index);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        serde_json::to_vec(&value).map(Bytes::from).unwrap_or_else(|error| {
            tracing::warn!(%error, "body patch serialization failed; preserving original bytes");
            raw.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_empty_patch_does_not_parse() {
        let raw = Bytes::from_static(b"{invalid JSON");
        let result = BodyPatchSet::default().apply(&raw);
        assert_eq!(result.as_ptr(), raw.as_ptr());
    }

    #[test]
    fn passthrough_model_patch_preserves_surrounding_bytes() {
        let raw =
            Bytes::from_static(b"{ \"unknown\": 1, \"model\" : \"old\", \"messages\" : [] }\n");
        let mut patches = BodyPatchSet::default();
        patches.set("/model", Value::from("new"), "model_mapping");
        assert_eq!(
            patches.apply(&raw),
            String::from_utf8_lossy(&raw)
                .replace("old", "new")
                .as_bytes()
        );
    }

    #[test]
    fn passthrough_patch_order_and_pointer_escaping() {
        let mut patches = BodyPatchSet::default();
        patches.set("/metadata/a~1b~0c", Value::from(1), "test");
        patches.set("/metadata/a~1b~0c", Value::from(2), "test");
        patches.remove("/thinking", "test");
        let result = patches.apply(&Bytes::from_static(b"{\"metadata\":{},\"thinking\":{}}"));
        assert_eq!(
            serde_json::from_slice::<Value>(&result).unwrap(),
            serde_json::json!({"metadata":{"a/b~c":2}})
        );
    }

    #[test]
    fn passthrough_patch_parse_failure_keeps_bytes() {
        let raw = Bytes::from_static(b"no JSON");
        let mut patches = BodyPatchSet::default();
        patches.remove("/messages", "test");
        assert_eq!(patches.apply(&raw), raw);
    }

    #[test]
    fn passthrough_patch_rejects_prefix_collision() {
        let result = std::panic::catch_unwind(|| {
            let mut patches = BodyPatchSet::default();
            patches.set("/model_private", Value::Null, "test");
            patches
        });
        if cfg!(debug_assertions) {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_empty());
        }
    }
}
