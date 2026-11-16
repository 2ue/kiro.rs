mod legacy_export;
mod legacy_import;
mod report;

pub use legacy_export::export_legacy;
pub use legacy_import::import_legacy;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyPackage {
    pub format_version: u32,
    pub exported_at: String,
    pub exported_from: ExportedFrom,
    pub accounts: Vec<Value>,
    pub runtime_config: Value,
    pub route_scenarios: Vec<Value>,
    pub model_pricing: Vec<Value>,
    pub model_capabilities: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportedFrom {
    pub kind: String,
    pub schema_fingerprint: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub secrets_redacted: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[cfg(test)]
#[path = "../../tests/migration/tests.rs"]
mod tests;
