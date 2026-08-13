//! Provider-neutral upstream account runtime types.
//!
//! This module is the target boundary for replacing the old local/external pool
//! split with upstream accounts as the scheduling unit.

pub mod account;
pub mod attempt;
pub mod runtime;
pub mod scheduler;
pub mod usage;

pub use account::{
    AccountAuthPolicy, AccountId, AccountLimits, AccountProxy, AccountSecretRef, UpstreamAccount,
};
pub use attempt::{
    AccountAttemptAction, AccountAttemptTrace, DeliveryEvidence, UpstreamErrorClass,
};
pub use runtime::{AccountRuntimeConfig, AccountRuntimeManager};
pub use scheduler::{AccountDispatchCandidate, AccountDispatchDecision, select_account_candidate};
pub use usage::{RawUsageFacts, TokenUsage, UsageConfidence};
