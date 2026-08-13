use serde::{Deserialize, Serialize};

use super::account::{AccountId, UpstreamAccount};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDispatchCandidate {
    pub account: UpstreamAccount,
    pub in_flight_requests: u32,
    pub recent_error_penalty: u32,
    pub cooling_down: bool,
}

impl AccountDispatchCandidate {
    fn dispatchable_for_model(&self, model: Option<&str>) -> bool {
        self.account.enabled
            && !self.cooling_down
            && self.account.supports_model(model)
            && self.has_concurrency_capacity()
    }

    fn has_concurrency_capacity(&self) -> bool {
        match self.account.limits.max_concurrent_requests {
            Some(0) | None => true,
            Some(max) => self.in_flight_requests < max,
        }
    }

    fn selection_key(&self) -> (u32, u32, u32, u64) {
        (
            self.account.limits.priority,
            self.recent_error_penalty,
            self.in_flight_requests,
            self.account.id.get(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDispatchDecision {
    pub account_id: AccountId,
    pub account_name: String,
}

pub fn select_account_candidate(
    candidates: &[AccountDispatchCandidate],
    model: Option<&str>,
) -> Option<AccountDispatchDecision> {
    let selected = candidates
        .iter()
        .filter(|candidate| candidate.dispatchable_for_model(model))
        .min_by_key(|candidate| candidate.selection_key())?;
    Some(AccountDispatchDecision {
        account_id: selected.account.id,
        account_name: selected.account.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_runtime::account::{
        AccountAuthPolicy, AccountLimits, AccountProxy, AccountSecretRef,
    };

    fn account(id: u64, name: &str, priority: u32, supported_models: Vec<&str>) -> UpstreamAccount {
        UpstreamAccount {
            id: AccountId::new(id).unwrap(),
            name: name.to_string(),
            enabled: true,
            base_url: "https://upstream.example.test".to_string(),
            auth: AccountAuthPolicy::Bearer {
                secret: AccountSecretRef::StoreRef(format!("secret/account/{id}")),
            },
            supported_models: supported_models.into_iter().map(str::to_string).collect(),
            limits: AccountLimits {
                priority,
                rpm: None,
                max_concurrent_requests: Some(2),
            },
            proxy: AccountProxy::Inherit,
        }
    }

    #[test]
    fn selection_prefers_dispatchable_lowest_priority_account() {
        let candidates = vec![
            AccountDispatchCandidate {
                account: account(2, "secondary", 20, vec!["claude-*"]),
                in_flight_requests: 0,
                recent_error_penalty: 0,
                cooling_down: false,
            },
            AccountDispatchCandidate {
                account: account(1, "primary", 10, vec!["claude-sonnet-*"]),
                in_flight_requests: 0,
                recent_error_penalty: 0,
                cooling_down: false,
            },
        ];
        let decision = select_account_candidate(&candidates, Some("claude-sonnet-4-6")).unwrap();
        assert_eq!(decision.account_id, AccountId::new(1).unwrap());
        assert_eq!(decision.account_name, "primary");
    }

    #[test]
    fn selection_skips_disabled_cooling_and_full_accounts() {
        let mut disabled = account(1, "disabled", 1, vec!["claude-*"]);
        disabled.enabled = false;
        let candidates = vec![
            AccountDispatchCandidate {
                account: disabled,
                in_flight_requests: 0,
                recent_error_penalty: 0,
                cooling_down: false,
            },
            AccountDispatchCandidate {
                account: account(2, "cooling", 2, vec!["claude-*"]),
                in_flight_requests: 0,
                recent_error_penalty: 0,
                cooling_down: true,
            },
            AccountDispatchCandidate {
                account: account(3, "full", 3, vec!["claude-*"]),
                in_flight_requests: 2,
                recent_error_penalty: 0,
                cooling_down: false,
            },
            AccountDispatchCandidate {
                account: account(4, "ready", 4, vec!["claude-*"]),
                in_flight_requests: 1,
                recent_error_penalty: 0,
                cooling_down: false,
            },
        ];
        let decision = select_account_candidate(&candidates, Some("claude-sonnet-4-6")).unwrap();
        assert_eq!(decision.account_id, AccountId::new(4).unwrap());
    }

    #[test]
    fn selection_returns_none_when_model_is_not_supported() {
        let candidates = vec![AccountDispatchCandidate {
            account: account(1, "primary", 0, vec!["claude-sonnet-*"]),
            in_flight_requests: 0,
            recent_error_penalty: 0,
            cooling_down: false,
        }];
        assert_eq!(
            select_account_candidate(&candidates, Some("claude-opus-4-1")),
            None
        );
    }
}
