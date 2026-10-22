//! Route scenarios: named snapshots of the route rules that already exist in four places
//! (local pool route policy, external pool route policy, per local account, per external
//! account). Applying a scenario writes those values back through the existing setters; the
//! scheduler is not involved.
//!
//! Applying is idempotent: each item is written to its target value, so a retry of the failed
//! items converges without touching the ones that succeeded.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::error::AdminServiceError;
use super::service::{AdminService, block_on_admin_store_public};
use super::types::SetCredentialRoutePolicyRequest;
use crate::external_pool::UpdateExternalPoolRequest;
use crate::model::config::{ExternalPoolRouteMode, normalize_route_rules};

const MAX_SCENARIO_NAME_CHARS: usize = 64;
const MAX_SCENARIO_DESCRIPTION_CHARS: usize = 500;
const MAX_SCENARIO_ACCOUNTS: usize = 5000;
const MAX_RULES_PER_POLICY: usize = 200;
const MAX_RULE_CHARS: usize = 256;

/// A route rule set: the same shape every route policy in the system uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePolicy {
    #[serde(default)]
    pub route_mode: ExternalPoolRouteMode,
    #[serde(default)]
    pub route_rules: Vec<String>,
}

impl RoutePolicy {
    fn normalized(&self) -> Self {
        let route_rules = if self.route_mode == ExternalPoolRouteMode::AllowAll {
            Vec::new()
        } else {
            normalize_route_rules(&self.route_rules)
        };
        Self {
            route_mode: self.route_mode,
            route_rules,
        }
    }

    fn unrestricted() -> Self {
        Self::default()
    }

    fn summary(&self) -> String {
        match self.route_mode {
            ExternalPoolRouteMode::AllowAll => "不限制".to_string(),
            ExternalPoolRouteMode::AllowList => format!("仅允许 {}", self.route_rules.join(", ")),
            ExternalPoolRouteMode::DenyList => format!("禁止 {}", self.route_rules.join(", ")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRoutePolicy {
    pub id: u64,
    #[serde(flatten)]
    pub policy: RoutePolicy,
}

/// Scenario body. Accounts not listed are reset to "unrestricted" when the scenario is applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioContent {
    #[serde(default)]
    pub local: RoutePolicy,
    #[serde(default)]
    pub external: RoutePolicy,
    #[serde(default)]
    pub local_accounts: Vec<AccountRoutePolicy>,
    #[serde(default)]
    pub external_accounts: Vec<AccountRoutePolicy>,
}

impl RouteScenarioContent {
    fn normalized(&self) -> Self {
        let dedup = |accounts: &[AccountRoutePolicy]| {
            let mut by_id = BTreeMap::new();
            for account in accounts {
                by_id.insert(account.id, account.policy.normalized());
            }
            by_id
                .into_iter()
                .map(|(id, policy)| AccountRoutePolicy { id, policy })
                .collect()
        };
        Self {
            local: self.local.normalized(),
            external: self.external.normalized(),
            local_accounts: dedup(&self.local_accounts),
            external_accounts: dedup(&self.external_accounts),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenario {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    pub content: RouteScenarioContent,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRouteScenarioRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: RouteScenarioContent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioListResponse {
    pub scenarios: Vec<RouteScenario>,
    /// Scenario most recently applied with every item succeeding, if any.
    pub active_scenario_id: Option<u64>,
    /// Whether the current settings still equal the active scenario.
    pub active_in_sync: bool,
}

/// What a scenario can select: the configured routes and the accounts it covers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioOptions {
    pub routes: Vec<String>,
    pub local_accounts: Vec<RouteScenarioAccountOption>,
    pub external_accounts: Vec<RouteScenarioAccountOption>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioAccountOption {
    pub id: u64,
    pub label: String,
    #[serde(flatten)]
    pub current: RoutePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteScenarioItemKind {
    LocalPool,
    ExternalPool,
    LocalAccount,
    ExternalAccount,
}

/// One write the scenario implies. `id` is 0 for the two pool-wide items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioItemKey {
    pub kind: RouteScenarioItemKind,
    #[serde(default)]
    pub id: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioChange {
    #[serde(flatten)]
    pub key: RouteScenarioItemKey,
    pub label: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioPreview {
    pub changes: Vec<RouteScenarioChange>,
    pub unchanged: usize,
    /// Scenario entries for accounts that no longer exist; skipped on apply.
    pub missing_accounts: Vec<RouteScenarioItemKey>,
    /// Rules that are not one of the selectable routes.
    pub unknown_routes: Vec<String>,
    /// Routes no local / external account would accept under the scenario.
    pub uncovered_local_routes: Vec<String>,
    pub uncovered_external_routes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRouteScenarioRequest {
    /// Only these items are written (retry of failed items). Empty applies everything.
    #[serde(default)]
    pub only: Vec<RouteScenarioItemKey>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioApplyItem {
    #[serde(flatten)]
    pub key: RouteScenarioItemKey,
    pub label: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteScenarioApplyResponse {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    /// Items already at the scenario value; nothing was written for them.
    pub skipped: usize,
    pub items: Vec<RouteScenarioApplyItem>,
}

/// Current state of every item a scenario covers.
struct RouteState {
    local: RoutePolicy,
    external: RoutePolicy,
    local_accounts: BTreeMap<u64, (String, RoutePolicy)>,
    external_accounts: BTreeMap<u64, (String, RoutePolicy)>,
}

struct PlannedItem {
    key: RouteScenarioItemKey,
    label: String,
    before: RoutePolicy,
    after: RoutePolicy,
}

fn validate_policy(policy: &RoutePolicy, label: &str) -> Result<(), AdminServiceError> {
    if policy.route_rules.len() > MAX_RULES_PER_POLICY {
        return Err(AdminServiceError::InvalidCredential(format!(
            "{label} 的路由不能超过 {MAX_RULES_PER_POLICY} 条"
        )));
    }
    if policy
        .route_rules
        .iter()
        .any(|rule| rule.len() > MAX_RULE_CHARS)
    {
        return Err(AdminServiceError::InvalidCredential(format!(
            "{label} 的路由单条长度不能超过 {MAX_RULE_CHARS}"
        )));
    }
    if policy.route_mode == ExternalPoolRouteMode::AllowList && policy.route_rules.is_empty() {
        return Err(AdminServiceError::InvalidCredential(format!(
            "{label} 选择了“仅允许”，至少要选一个路由"
        )));
    }
    Ok(())
}

fn validate_request(
    req: &SaveRouteScenarioRequest,
) -> Result<(String, Option<String>, RouteScenarioContent), AdminServiceError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AdminServiceError::InvalidCredential(
            "场景名称不能为空".to_string(),
        ));
    }
    if name.chars().count() > MAX_SCENARIO_NAME_CHARS {
        return Err(AdminServiceError::InvalidCredential(format!(
            "场景名称不能超过 {MAX_SCENARIO_NAME_CHARS} 个字符"
        )));
    }
    let description = req
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if description
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_SCENARIO_DESCRIPTION_CHARS)
    {
        return Err(AdminServiceError::InvalidCredential(format!(
            "场景说明不能超过 {MAX_SCENARIO_DESCRIPTION_CHARS} 个字符"
        )));
    }
    let content = req.content.normalized();
    if content.local_accounts.len() + content.external_accounts.len() > MAX_SCENARIO_ACCOUNTS {
        return Err(AdminServiceError::InvalidCredential(format!(
            "单个场景最多包含 {MAX_SCENARIO_ACCOUNTS} 个账号规则"
        )));
    }
    validate_policy(&content.local, "本地整体")?;
    validate_policy(&content.external, "外部整体")?;
    for account in &content.local_accounts {
        validate_policy(&account.policy, &format!("本地账号 #{}", account.id))?;
    }
    for account in &content.external_accounts {
        validate_policy(&account.policy, &format!("外部账号 #{}", account.id))?;
    }
    Ok((name, description, content))
}

fn scenario_from_row(row: crate::storage::postgres::RouteScenarioRow) -> RouteScenario {
    let content = serde_json::from_value::<RouteScenarioContent>(row.content)
        .unwrap_or_default()
        .normalized();
    RouteScenario {
        id: row.id,
        name: row.name,
        description: row.description,
        content,
        revision: row.revision,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// Whether a route policy accepts `route` (a selectable route prefix).
fn policy_accepts(policy: &RoutePolicy, route: &str) -> bool {
    crate::model::config::route_policy_allows(policy.route_mode, &policy.route_rules, route)
}

fn plan_items(
    content: &RouteScenarioContent,
    state: &RouteState,
) -> (Vec<PlannedItem>, Vec<RouteScenarioItemKey>) {
    let mut items = vec![
        PlannedItem {
            key: RouteScenarioItemKey {
                kind: RouteScenarioItemKind::LocalPool,
                id: 0,
            },
            label: "本地整体".to_string(),
            before: state.local.clone(),
            after: content.local.clone(),
        },
        PlannedItem {
            key: RouteScenarioItemKey {
                kind: RouteScenarioItemKind::ExternalPool,
                id: 0,
            },
            label: "外部整体".to_string(),
            before: state.external.clone(),
            after: content.external.clone(),
        },
    ];
    let mut missing = Vec::new();
    for (kind, listed, current) in [
        (
            RouteScenarioItemKind::LocalAccount,
            &content.local_accounts,
            &state.local_accounts,
        ),
        (
            RouteScenarioItemKind::ExternalAccount,
            &content.external_accounts,
            &state.external_accounts,
        ),
    ] {
        let listed: BTreeMap<u64, &RoutePolicy> = listed
            .iter()
            .map(|account| (account.id, &account.policy))
            .collect();
        for id in listed.keys().filter(|id| !current.contains_key(id)) {
            missing.push(RouteScenarioItemKey { kind, id: *id });
        }
        for (id, (label, before)) in current {
            let after = listed
                .get(id)
                .map(|policy| (*policy).clone())
                .unwrap_or_else(RoutePolicy::unrestricted);
            items.push(PlannedItem {
                key: RouteScenarioItemKey { kind, id: *id },
                label: label.clone(),
                before: before.clone(),
                after,
            });
        }
    }
    (items, missing)
}

impl AdminService {
    fn route_state(&self) -> Result<RouteState, AdminServiceError> {
        let config = self.route_scenario_runtime_config();
        let pools = self.list_external_pools()?;
        let local_accounts = self
            .route_scenario_local_accounts()
            .into_iter()
            .map(|(id, label, policy)| (id, (label, policy.normalized())))
            .collect();
        let external_accounts = pools
            .into_iter()
            .map(|pool| {
                let label = format!("{} · {}", pool.name, pool.base_url);
                let policy = RoutePolicy {
                    route_mode: pool.route_mode,
                    route_rules: pool.route_rules,
                }
                .normalized();
                (pool.id, (label, policy))
            })
            .collect();
        Ok(RouteState {
            local: RoutePolicy {
                route_mode: config.local_pool_route_mode,
                route_rules: config.local_pool_route_rules,
            }
            .normalized(),
            external: RoutePolicy {
                route_mode: config.external_pool_route_mode,
                route_rules: config.external_pool_route_rules,
            }
            .normalized(),
            local_accounts,
            external_accounts,
        })
    }

    /// Selectable routes (the route policies listed on the route policy page) and the accounts
    /// a scenario covers, with their current rules.
    pub fn route_scenario_options(&self) -> Result<RouteScenarioOptions, AdminServiceError> {
        let state = self.route_state()?;
        let accounts = |accounts: BTreeMap<u64, (String, RoutePolicy)>| {
            accounts
                .into_iter()
                .map(|(id, (label, current))| RouteScenarioAccountOption { id, label, current })
                .collect()
        };
        Ok(RouteScenarioOptions {
            routes: self.route_scenario_routes(),
            local_accounts: accounts(state.local_accounts),
            external_accounts: accounts(state.external_accounts),
        })
    }

    pub fn list_route_scenarios(&self) -> Result<RouteScenarioListResponse, AdminServiceError> {
        let store = self.route_scenario_store();
        let rows = block_on_admin_store_public(async move { store.list_route_scenarios().await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let scenarios: Vec<RouteScenario> = rows.into_iter().map(scenario_from_row).collect();
        let active_scenario_id = self.route_scenario_active_id();
        let active_in_sync = match active_scenario_id
            .and_then(|id| scenarios.iter().find(|scenario| scenario.id == id))
        {
            Some(scenario) => {
                let state = self.route_state()?;
                let (items, _) = plan_items(&scenario.content, &state);
                items.iter().all(|item| item.before == item.after)
            }
            None => false,
        };
        Ok(RouteScenarioListResponse {
            scenarios,
            active_scenario_id,
            active_in_sync,
        })
    }

    fn get_route_scenario(&self, id: u64) -> Result<RouteScenario, AdminServiceError> {
        let store = self.route_scenario_store();
        let row = block_on_admin_store_public(async move { store.get_route_scenario(id).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        Ok(scenario_from_row(row))
    }

    pub fn create_route_scenario(
        &self,
        req: SaveRouteScenarioRequest,
    ) -> Result<RouteScenario, AdminServiceError> {
        let (name, description, content) = validate_request(&req)?;
        let value = serde_json::to_value(&content)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let store = self.route_scenario_store();
        let row = block_on_admin_store_public(async move {
            store
                .insert_route_scenario(&name, description.as_deref(), &value)
                .await
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let scenario = scenario_from_row(row);
        self.route_scenario_audit(
            "create_route_scenario",
            scenario.id,
            json!({ "name": scenario.name }),
        );
        Ok(scenario)
    }

    pub fn update_route_scenario(
        &self,
        id: u64,
        req: SaveRouteScenarioRequest,
    ) -> Result<RouteScenario, AdminServiceError> {
        let (name, description, content) = validate_request(&req)?;
        let value = serde_json::to_value(&content)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let store = self.route_scenario_store();
        let row = block_on_admin_store_public(async move {
            store
                .update_route_scenario(id, &name, description.as_deref(), &value)
                .await
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
        .ok_or(AdminServiceError::NotFound { id })?;
        let scenario = scenario_from_row(row);
        self.route_scenario_audit(
            "update_route_scenario",
            id,
            json!({ "name": scenario.name }),
        );
        Ok(scenario)
    }

    pub fn delete_route_scenario(&self, id: u64) -> Result<(), AdminServiceError> {
        let store = self.route_scenario_store();
        let deleted =
            block_on_admin_store_public(async move { store.delete_route_scenario(id).await })
                .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        if !deleted {
            return Err(AdminServiceError::NotFound { id });
        }
        if self.route_scenario_active_id() == Some(id) {
            self.set_route_scenario_active_id(None);
        }
        self.route_scenario_audit("delete_route_scenario", id, json!({}));
        Ok(())
    }

    /// Current settings as scenario content, for "save current settings as a scenario".
    pub fn current_route_scenario_content(
        &self,
    ) -> Result<RouteScenarioContent, AdminServiceError> {
        let state = self.route_state()?;
        let restricted = |accounts: &BTreeMap<u64, (String, RoutePolicy)>| {
            accounts
                .iter()
                .filter(|(_, (_, policy))| policy.route_mode != ExternalPoolRouteMode::AllowAll)
                .map(|(id, (_, policy))| AccountRoutePolicy {
                    id: *id,
                    policy: policy.clone(),
                })
                .collect()
        };
        Ok(RouteScenarioContent {
            local: state.local.clone(),
            external: state.external.clone(),
            local_accounts: restricted(&state.local_accounts),
            external_accounts: restricted(&state.external_accounts),
        })
    }

    pub fn preview_route_scenario(
        &self,
        id: u64,
    ) -> Result<RouteScenarioPreview, AdminServiceError> {
        let scenario = self.get_route_scenario(id)?;
        let state = self.route_state()?;
        let (items, missing_accounts) = plan_items(&scenario.content, &state);
        let routes = self.route_scenario_routes();
        let known: BTreeSet<&str> = routes.iter().map(String::as_str).collect();

        let mut unknown_routes = BTreeSet::new();
        for item in &items {
            for rule in &item.after.route_rules {
                if !known.contains(rule.as_str()) {
                    unknown_routes.insert(rule.clone());
                }
            }
        }

        let after_of = |kind: RouteScenarioItemKind| {
            items
                .iter()
                .filter(move |item| item.key.kind == kind)
                .map(|item| &item.after)
        };
        let pool_after = |kind: RouteScenarioItemKind| {
            items
                .iter()
                .find(|item| item.key.kind == kind)
                .map(|item| item.after.clone())
                .unwrap_or_default()
        };
        let local_pool = pool_after(RouteScenarioItemKind::LocalPool);
        let external_pool = pool_after(RouteScenarioItemKind::ExternalPool);
        let uncovered = |pool: &RoutePolicy, kind: RouteScenarioItemKind| -> Vec<String> {
            routes
                .iter()
                .filter(|route| policy_accepts(pool, route))
                .filter(|route| !after_of(kind).any(|policy| policy_accepts(policy, route)))
                .cloned()
                .collect()
        };
        let uncovered_local_routes = uncovered(&local_pool, RouteScenarioItemKind::LocalAccount);
        let uncovered_external_routes =
            uncovered(&external_pool, RouteScenarioItemKind::ExternalAccount);

        let mut changes = Vec::new();
        let mut unchanged = 0;
        for item in items {
            if item.before == item.after {
                unchanged += 1;
            } else {
                changes.push(RouteScenarioChange {
                    key: item.key,
                    label: item.label,
                    before: item.before.summary(),
                    after: item.after.summary(),
                });
            }
        }
        Ok(RouteScenarioPreview {
            changes,
            unchanged,
            missing_accounts,
            unknown_routes: unknown_routes.into_iter().collect(),
            uncovered_local_routes,
            uncovered_external_routes,
        })
    }

    /// Writes the scenario item by item and reports every result. Items already at their target
    /// value are skipped. `only` restricts the run to the listed items (retry of failures).
    pub fn apply_route_scenario(
        &self,
        id: u64,
        req: ApplyRouteScenarioRequest,
    ) -> Result<RouteScenarioApplyResponse, AdminServiceError> {
        let scenario = self.get_route_scenario(id)?;
        let state = self.route_state()?;
        let (items, _) = plan_items(&scenario.content, &state);
        let selected: Vec<PlannedItem> = if req.only.is_empty() {
            items
        } else {
            items
                .into_iter()
                .filter(|item| req.only.contains(&item.key))
                .collect()
        };

        let total = selected.len();
        let mut results = Vec::new();
        let mut skipped = 0;
        let mut pool_update: Option<(RoutePolicy, RoutePolicy)> = None;
        let mut pool_items = Vec::new();
        for item in selected {
            if item.before == item.after {
                skipped += 1;
                continue;
            }
            match item.key.kind {
                RouteScenarioItemKind::LocalPool | RouteScenarioItemKind::ExternalPool => {
                    let entry = pool_update
                        .get_or_insert_with(|| (state.local.clone(), state.external.clone()));
                    if item.key.kind == RouteScenarioItemKind::LocalPool {
                        entry.0 = item.after.clone();
                    } else {
                        entry.1 = item.after.clone();
                    }
                    pool_items.push(item);
                }
                RouteScenarioItemKind::LocalAccount => {
                    let result = self.set_credential_route_policy(
                        item.key.id,
                        SetCredentialRoutePolicyRequest {
                            route_mode: item.after.route_mode,
                            route_rules: item.after.route_rules.clone(),
                        },
                    );
                    results.push(apply_item(item, result));
                }
                RouteScenarioItemKind::ExternalAccount => {
                    let result = self
                        .update_external_pool(
                            item.key.id,
                            UpdateExternalPoolRequest {
                                route_mode: Some(item.after.route_mode),
                                route_rules: Some(item.after.route_rules.clone()),
                                ..UpdateExternalPoolRequest::default()
                            },
                        )
                        .map(|_| ());
                    results.push(apply_item(item, result));
                }
            }
        }
        if let Some((local, external)) = pool_update {
            let error = self
                .set_pool_route_policies(&local, &external)
                .err()
                .map(|err| err.to_string());
            for item in pool_items {
                results.push(item_result(item, error.clone()));
            }
        }

        let success = results.iter().filter(|item| item.success).count();
        let failed = results.len() - success;
        // The scenario becomes the active one once every item is written: directly for a full
        // run without failures, and for a retry run when nothing is left to change.
        if failed == 0 {
            let fully_applied = req.only.is_empty()
                || self.route_state().is_ok_and(|state| {
                    plan_items(&scenario.content, &state)
                        .0
                        .iter()
                        .all(|item| item.before == item.after)
                });
            if fully_applied {
                self.set_route_scenario_active_id(Some(id));
            }
        }
        self.route_scenario_audit(
            "apply_route_scenario",
            id,
            json!({ "name": scenario.name, "success": success, "failed": failed, "skipped": skipped, "retry": !req.only.is_empty() }),
        );
        Ok(RouteScenarioApplyResponse {
            total,
            success,
            failed,
            skipped,
            items: results,
        })
    }
}

fn apply_item(item: PlannedItem, result: Result<(), AdminServiceError>) -> RouteScenarioApplyItem {
    item_result(item, result.err().map(|err| err.to_string()))
}

fn item_result(item: PlannedItem, error: Option<String>) -> RouteScenarioApplyItem {
    RouteScenarioApplyItem {
        key: item.key,
        label: item.label,
        success: error.is_none(),
        error,
    }
}

// ============ HTTP handlers ============

pub(super) mod http {
    use axum::{
        Json,
        extract::{Path, State},
        response::{IntoResponse, Response},
    };

    use super::{ApplyRouteScenarioRequest, SaveRouteScenarioRequest};
    use crate::admin::error::AdminServiceError;
    use crate::admin::middleware::AdminState;
    use crate::admin::types::SuccessResponse;

    fn respond<T: serde::Serialize>(result: Result<T, AdminServiceError>) -> Response {
        match result {
            Ok(value) => Json(value).into_response(),
            Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
        }
    }

    /// GET /api/admin/route-scenarios
    pub async fn list(State(state): State<AdminState>) -> Response {
        respond(state.service.list_route_scenarios())
    }

    /// GET /api/admin/route-scenarios/options
    pub async fn options(State(state): State<AdminState>) -> Response {
        respond(state.service.route_scenario_options())
    }

    /// GET /api/admin/route-scenarios/current
    pub async fn current(State(state): State<AdminState>) -> Response {
        respond(state.service.current_route_scenario_content())
    }

    /// POST /api/admin/route-scenarios
    pub async fn create(
        State(state): State<AdminState>,
        Json(payload): Json<SaveRouteScenarioRequest>,
    ) -> Response {
        respond(state.service.create_route_scenario(payload))
    }

    /// PUT /api/admin/route-scenarios/{id}
    pub async fn update(
        State(state): State<AdminState>,
        Path(id): Path<u64>,
        Json(payload): Json<SaveRouteScenarioRequest>,
    ) -> Response {
        respond(state.service.update_route_scenario(id, payload))
    }

    /// DELETE /api/admin/route-scenarios/{id}
    pub async fn delete(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
        respond(
            state
                .service
                .delete_route_scenario(id)
                .map(|()| SuccessResponse::new("路由场景已删除")),
        )
    }

    /// GET /api/admin/route-scenarios/{id}/preview
    pub async fn preview(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
        respond(state.service.preview_route_scenario(id))
    }

    /// POST /api/admin/route-scenarios/{id}/apply
    pub async fn apply(
        State(state): State<AdminState>,
        Path(id): Path<u64>,
        payload: Option<Json<ApplyRouteScenarioRequest>>,
    ) -> Response {
        let request = payload.map(|Json(request)| request).unwrap_or_default();
        respond(state.service.apply_route_scenario(id, request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: ExternalPoolRouteMode, rules: &[&str]) -> RoutePolicy {
        RoutePolicy {
            route_mode: mode,
            route_rules: rules.iter().map(|rule| rule.to_string()).collect(),
        }
    }

    fn state() -> RouteState {
        RouteState {
            local: RoutePolicy::unrestricted(),
            external: RoutePolicy::unrestricted(),
            local_accounts: BTreeMap::from([
                (1, ("a".to_string(), RoutePolicy::unrestricted())),
                (
                    2,
                    (
                        "b".to_string(),
                        policy(ExternalPoolRouteMode::DenyList, &["/cc"]),
                    ),
                ),
                (3, ("c".to_string(), RoutePolicy::unrestricted())),
            ]),
            external_accounts: BTreeMap::from([(
                10,
                ("pool".to_string(), RoutePolicy::unrestricted()),
            )]),
        }
    }

    #[test]
    fn unlisted_accounts_reset_to_unrestricted_and_missing_ones_are_reported() {
        let content = RouteScenarioContent {
            local: policy(ExternalPoolRouteMode::DenyList, &["/ha"]),
            local_accounts: vec![
                AccountRoutePolicy {
                    id: 1,
                    policy: policy(ExternalPoolRouteMode::AllowList, &["/v1"]),
                },
                AccountRoutePolicy {
                    id: 99,
                    policy: policy(ExternalPoolRouteMode::AllowList, &["/v1"]),
                },
            ],
            ..Default::default()
        };
        let (items, missing) = plan_items(&content, &state());
        let after = |kind, id| {
            items
                .iter()
                .find(|item| item.key == RouteScenarioItemKey { kind, id })
                .unwrap()
        };
        assert_eq!(
            after(RouteScenarioItemKind::LocalPool, 0).after,
            policy(ExternalPoolRouteMode::DenyList, &["/ha"])
        );
        assert_eq!(
            after(RouteScenarioItemKind::LocalAccount, 1).after,
            policy(ExternalPoolRouteMode::AllowList, &["/v1"])
        );
        // Account 2 had a rule but is not listed: reset to unrestricted.
        assert_eq!(
            after(RouteScenarioItemKind::LocalAccount, 2).after,
            RoutePolicy::unrestricted()
        );
        assert_eq!(
            after(RouteScenarioItemKind::ExternalAccount, 10).after,
            RoutePolicy::unrestricted()
        );
        assert_eq!(
            missing,
            vec![RouteScenarioItemKey {
                kind: RouteScenarioItemKind::LocalAccount,
                id: 99
            }]
        );
        // 2 pool items + 3 local + 1 external; the missing account is not written.
        assert_eq!(items.len(), 6);
    }

    #[test]
    fn content_normalization_dedups_accounts_and_drops_rules_of_unrestricted_policies() {
        let content = RouteScenarioContent {
            local: policy(ExternalPoolRouteMode::AllowAll, &["/cc"]),
            local_accounts: vec![
                AccountRoutePolicy {
                    id: 1,
                    policy: policy(ExternalPoolRouteMode::DenyList, &["/CC/"]),
                },
                AccountRoutePolicy {
                    id: 1,
                    policy: policy(ExternalPoolRouteMode::DenyList, &["/ha"]),
                },
            ],
            ..Default::default()
        }
        .normalized();
        assert!(content.local.route_rules.is_empty());
        assert_eq!(content.local_accounts.len(), 1);
        assert_eq!(
            content.local_accounts[0].policy.route_rules,
            vec!["/ha".to_string()]
        );
    }

    #[test]
    fn validation_rejects_empty_allow_list_and_blank_names() {
        let mut req = SaveRouteScenarioRequest {
            name: "  ".to_string(),
            description: None,
            content: RouteScenarioContent::default(),
        };
        assert!(validate_request(&req).is_err());
        req.name = "日常".to_string();
        assert!(validate_request(&req).is_ok());
        req.content.local_accounts = vec![AccountRoutePolicy {
            id: 1,
            policy: policy(ExternalPoolRouteMode::AllowList, &[]),
        }];
        assert!(validate_request(&req).is_err());
        req.content.local_accounts = vec![AccountRoutePolicy {
            id: 1,
            policy: RoutePolicy {
                route_mode: ExternalPoolRouteMode::DenyList,
                route_rules: vec![format!("/{}", "x".repeat(300))],
            },
        }];
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn content_round_trips_through_json_with_flat_account_entries() {
        let content = RouteScenarioContent {
            local_accounts: vec![AccountRoutePolicy {
                id: 7,
                policy: policy(ExternalPoolRouteMode::DenyList, &["/cc"]),
            }],
            ..Default::default()
        };
        let value = serde_json::to_value(&content).unwrap();
        assert_eq!(
            value["localAccounts"][0],
            json!({"id": 7, "routeMode": "deny_list", "routeRules": ["/cc"]})
        );
        assert_eq!(
            serde_json::from_value::<RouteScenarioContent>(value).unwrap(),
            content
        );
    }
}
