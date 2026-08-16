//! Provider-neutral upstream account runtime types.
//!
//! This module is the target boundary for replacing the legacy split scheduler
//! with upstream accounts as the scheduling unit.

pub mod account;
pub mod attempt;
pub mod migration;
pub mod runtime;
pub mod scheduler;
pub(crate) mod storage_task {
    pub(crate) use crate::local_upstream::storage_task::{
        drain_local_upstream_storage_tasks as drain_account_runtime_storage_tasks,
        local_upstream_storage_task_stats as account_runtime_storage_task_stats,
        shutdown_local_upstream_storage_tasks as shutdown_account_runtime_storage_tasks,
        spawn_local_upstream_critical_storage_task as spawn_account_runtime_critical_storage_task,
    };
}
pub mod store;
pub mod usage;

pub use account::{
    AccountAuthPolicy, AccountId, AccountLimits, AccountProxy, AccountSecretRef, UpstreamAccount,
};
pub use attempt::{
    AccountAttemptAction, AccountAttemptTrace, DeliveryEvidence, UpstreamErrorClass,
};
pub use migration::upstream_account_from_external_pool;
pub use runtime::{
    AccountAuthType, AccountAutoDisablePolicy, AccountFinalError, AccountForwardOutcome,
    AccountLatencyTraceState, AccountModelMappingMode, AccountRawModelMode, AccountRequestBodyMode,
    AccountRouteMode, AccountRouteRequest, AccountRuntimeConfig, AccountRuntimeConfigExt,
    AccountRuntimeManager, AccountStreamResponseMode, AccountStreamRetryMode,
    AccountUsageProjectionMode, UpstreamAccountStatusRecord, UpstreamAccountStorageRecord,
    account_direct_policy_reason, cached_eligible_account_for_route_and_model,
    cached_eligible_account_for_route_body_mode_and_model,
    cached_immediately_available_account_for_route_and_model,
    cached_immediately_available_account_for_route_body_mode_and_model,
    clear_upstream_account_cooldowns, eligible_account_for_route_and_model,
    eligible_account_for_route_body_mode_and_model, forward_account_with_failover,
    forward_account_with_failover_result, immediately_available_account_for_route_and_model,
    immediately_available_account_for_route_body_mode_and_model,
    load_upstream_account_status_records, upstream_account_messages_url,
    upstream_account_models_url,
};
pub(crate) use runtime::{
    AccountRouteRequestPreparationCache, AccountRuntimeStatusCompatibilityResponse,
    UpstreamAccountEligibility, mask_upstream_account_key,
    normalize_upstream_account_model_mapping_rules,
};
pub use scheduler::{AccountDispatchCandidate, AccountDispatchDecision, select_account_candidate};
pub use store::{CreateUpstreamAccountStorageRequest, UpdateUpstreamAccountStorageRequest};
pub use usage::{RawUsageFacts, TokenUsage, UsageConfidence};
