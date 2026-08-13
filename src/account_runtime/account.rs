use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(u64);

impl AccountId {
    pub fn new(value: u64) -> Option<Self> {
        (value > 0).then_some(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AccountSecretRef {
    InlineRedacted(String),
    StoreRef(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AccountAuthPolicy {
    Bearer {
        secret: AccountSecretRef,
    },
    Header {
        name: String,
        secret: AccountSecretRef,
    },
    StaticHeaders {
        headers: Vec<(String, AccountSecretRef)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AccountProxy {
    Inherit,
    Direct,
    Url {
        url: String,
        username: Option<String>,
        password: Option<AccountSecretRef>,
    },
    Resource {
        id: u64,
    },
}

impl Default for AccountProxy {
    fn default() -> Self {
        Self::Inherit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLimits {
    pub priority: u32,
    pub rpm: Option<u32>,
    pub max_concurrent_requests: Option<u32>,
}

impl Default for AccountLimits {
    fn default() -> Self {
        Self {
            priority: 0,
            rpm: None,
            max_concurrent_requests: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamAccount {
    pub id: AccountId,
    pub name: String,
    pub enabled: bool,
    pub base_url: String,
    pub auth: AccountAuthPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_models: Vec<String>,
    #[serde(default)]
    pub limits: AccountLimits,
    #[serde(default)]
    pub proxy: AccountProxy,
}

impl UpstreamAccount {
    pub fn supports_model(&self, requested_model: Option<&str>) -> bool {
        if self.supported_models.is_empty() {
            return true;
        }
        let Some(requested_model) = requested_model
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            return true;
        };
        self.supported_models.iter().any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*"
                || candidate.eq_ignore_ascii_case(requested_model)
                || wildcard_model_match(candidate, requested_model)
        })
    }
}

fn wildcard_model_match(pattern: &str, requested_model: &str) -> bool {
    let Some(prefix) = pattern.strip_suffix('*') else {
        return false;
    };
    !prefix.is_empty() && requested_model.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_account(supported_models: Vec<String>) -> UpstreamAccount {
        UpstreamAccount {
            id: AccountId::new(1).unwrap(),
            name: "primary".to_string(),
            enabled: true,
            base_url: "https://upstream.example.test".to_string(),
            auth: AccountAuthPolicy::Bearer {
                secret: AccountSecretRef::StoreRef("secret/account/1".to_string()),
            },
            supported_models,
            limits: AccountLimits::default(),
            proxy: AccountProxy::default(),
        }
    }

    #[test]
    fn account_id_rejects_zero() {
        assert_eq!(AccountId::new(0), None);
        assert_eq!(AccountId::new(42).unwrap().get(), 42);
    }

    #[test]
    fn empty_model_list_allows_any_requested_model() {
        let account = test_account(Vec::new());
        assert!(account.supports_model(Some("claude-sonnet-4-6")));
        assert!(account.supports_model(None));
    }

    #[test]
    fn explicit_model_list_matches_case_insensitively_and_with_prefix_wildcard() {
        let account = test_account(vec![
            "claude-sonnet-*".to_string(),
            "CLAUDE-OPUS-4-1".to_string(),
        ]);
        assert!(account.supports_model(Some("claude-sonnet-4-6")));
        assert!(account.supports_model(Some("claude-opus-4-1")));
        assert!(!account.supports_model(Some("claude-haiku-4-5")));
    }
}
