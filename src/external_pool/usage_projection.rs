use super::*;

#[derive(Debug, Default)]
struct ExternalUsageProjectionState {
    committed_controlled_usage: Option<CacheUsage>,
}

#[derive(Clone)]
pub(super) struct ExternalUsageProjectionContext {
    pub(super) mode: ExternalPoolUsageProjectionMode,
    pub(super) raw_input_tokens: i32,
    pub(super) cache_state_enabled: bool,
    pub(super) credential_key: Option<String>,
    pub(super) model: String,
    pub(super) simulated_usage: Option<CacheSimulation>,
    pub(super) reported_policy: Option<ReportedCacheUsagePolicy>,
    pub(super) scope: Option<PromptCacheScope>,
    pub(super) prompt_cache: Arc<PromptCacheTracker>,
    pub(super) prompt_cache_profile: Option<PromptCacheProfile>,
    pub(super) kiro_rs_tool_prompt_cache_plan: Option<KiroRsToolPromptCachePlan>,
    pub(super) prompt_cache_target_read_ratio: f64,
    pub(super) prompt_cache_bounds: PromptCacheBounds,
    pub(super) prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub(super) prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub(super) uplift_percent: u32,
    pub(super) output_uplift_min_tokens: i32,
    pub(super) output_uplift_percent: u32,
    state: Arc<SyncMutex<ExternalUsageProjectionState>>,
}

pub(super) fn build_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    if pool.usage_projection_mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }
    let stream = route.is_stream();
    let reported_usage = route.reported_usage.policy_for_path(&route.endpoint);
    if !stream && reported_usage.skip_non_stream_usage_projection {
        return None;
    }
    if !reported_usage.enabled
        && route.prompt_cache_strategy_type != PromptCacheStrategyType::NoCache
    {
        return None;
    }

    let raw_projection_payload;
    let payload = if let Some(payload) = route.payload() {
        payload
    } else {
        raw_projection_payload = serde_json::from_slice::<MessagesRequest>(&route.raw_body).ok()?;
        if raw_projection_payload.model.trim().is_empty() {
            return None;
        }
        &raw_projection_payload
    };

    let model = route
        .upstream_model
        .clone()
        .unwrap_or_else(|| payload.model.clone());
    let prompt_cache_supported = route
        .model_capabilities
        .supports_prompt_caching_for(&model)
        .unwrap_or(true);

    let raw_input_tokens = if route.request_input_tokens > 0 {
        route.request_input_tokens
    } else {
        token::count_all_tokens(
            &payload.model,
            payload.system.as_deref(),
            &payload.messages,
            payload.tools.as_deref(),
        ) as i32
    };
    let cache_state_enabled = route.prompt_cache_strategy_type != PromptCacheStrategyType::NoCache
        && prompt_cache_supported;
    let stable_conversation_id = route
        .stable_conversation_id()
        .or_else(|| crate::anthropic::converter::extract_stable_conversation_id(payload));
    let scope = cache_state_enabled
        .then(|| {
            prompt_cache_scope(
                stable_conversation_id.clone(),
                route.prompt_cache_route_namespace.clone(),
            )
        })
        .flatten();
    let (profile, kiro_rs_tool_prompt_cache_plan, simulated_usage) = match route
        .prompt_cache_strategy_type
    {
        PromptCacheStrategyType::CurrentHighCache
            if prompt_cache_supported
                && route.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache =>
        {
            let profile = route.prompt_cache.build_high_cache_profile_for_model(
                payload,
                raw_input_tokens,
                &model,
            );
            let prompt_usage = route.prompt_cache.compute_with_bounds(
                scope.clone(),
                profile.as_ref(),
                route.prompt_cache_target_read_ratio,
                route.prompt_cache_bounds,
            );
            let simulated_usage = profile.as_ref().and_then(|profile| {
                CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
                    prompt_usage,
                    route.prompt_cache_target_read_ratio,
                    cache_amplification(route, profile),
                )
            });
            (profile, None, simulated_usage)
        }
        PromptCacheStrategyType::KiroRsTool if prompt_cache_supported => {
            let plan = route.prompt_cache.compute_kiro_rs_tool_with_bounds(
                scope.clone(),
                payload,
                raw_input_tokens,
                &model,
                route.prompt_cache_bounds,
                route.kiro_rs_tool_cache_policy,
            );
            let policy = route.kiro_rs_tool_cache_policy.normalized();
            let simulated_usage =
                CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(
                    plan.usage(),
                    policy.reported_input_min_tokens,
                    policy.reported_input_max_tokens,
                    plan.cache_jitter_seed(),
                );
            (None, Some(plan), simulated_usage)
        }
        _ => (None, None, None),
    };
    let reported_policy = match route.prompt_cache_strategy_type {
        PromptCacheStrategyType::NoCache => ReportedCacheUsagePolicy::from_path_policy(
            crate::model::config::ReportedUsagePathPolicy::disabled(),
            fastrand::u64(..),
        ),
        PromptCacheStrategyType::CurrentHighCache
            if prompt_cache_supported
                && route.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache =>
        {
            ReportedCacheUsagePolicy::from_path_policy(
                reported_usage,
                profile
                    .as_ref()
                    .map(|profile| profile.cache_jitter_seed())
                    .unwrap_or(0)
                    ^ fastrand::u64(..),
            )
        }
        PromptCacheStrategyType::KiroRsTool if prompt_cache_supported => {
            ReportedCacheUsagePolicy::from_path_policy(reported_usage, fastrand::u64(..))
        }
        PromptCacheStrategyType::CurrentHighCache | PromptCacheStrategyType::KiroRsTool => None,
    };
    Some(ExternalUsageProjectionContext {
        mode: pool.usage_projection_mode,
        raw_input_tokens,
        cache_state_enabled,
        credential_key: Some(format!("external_pool:{}", pool.id)),
        model,
        simulated_usage,
        reported_policy,
        scope,
        prompt_cache: route.prompt_cache.clone(),
        prompt_cache_profile: profile,
        kiro_rs_tool_prompt_cache_plan,
        prompt_cache_target_read_ratio: route.prompt_cache_target_read_ratio,
        prompt_cache_bounds: route.prompt_cache_bounds,
        prompt_cache_creation_controller: route.prompt_cache_creation_controller.clone(),
        prompt_cache_creation_control: route.prompt_cache_creation_control,
        uplift_percent,
        output_uplift_min_tokens: output_uplift_min_tokens.max(0),
        output_uplift_percent: output_uplift_percent.min(200),
        state: Arc::new(SyncMutex::new(ExternalUsageProjectionState::default())),
    })
}

fn prompt_cache_scope(
    conversation_id: Option<String>,
    namespace: Option<String>,
) -> Option<PromptCacheScope> {
    Some(PromptCacheScope::new(conversation_id?, namespace))
}

fn cache_amplification(
    route: &ExternalRouteRequest,
    profile: &PromptCacheProfile,
) -> Option<CacheAmplification> {
    Some(CacheAmplification::new(
        route.prompt_cache_token_scale,
        route.prompt_cache_max_simulated_input_tokens,
        route.prompt_cache_cap_jitter_min_tokens,
        route.prompt_cache_cap_jitter_max_tokens,
        route.prompt_cache_scale_min_input_tokens,
        profile.cache_jitter_seed(),
    ))
}

impl ExternalUsageProjectionContext {
    pub(super) fn mark_committed(&self, usage: CacheUsage) {
        let mut state = self.state.lock();
        state.committed_controlled_usage = Some(usage);
    }

    pub(super) fn record_success(&self) {
        let Some(usage) = self.state.lock().committed_controlled_usage else {
            return;
        };
        if !self.cache_state_enabled {
            return;
        }
        if self.reported_policy.is_some() {
            let _ = self
                .prompt_cache_creation_controller
                .apply_success_with_context(
                    self.scope.as_ref(),
                    self.prompt_cache_creation_control,
                    usage,
                    self.credential_key.as_deref(),
                    Some(self.model.as_str()),
                );
        }
        if let Some(plan) = self.kiro_rs_tool_prompt_cache_plan.as_ref() {
            self.prompt_cache.commit_kiro_rs_tool_success_with_bounds(
                self.scope.clone(),
                plan,
                self.prompt_cache_bounds,
            );
        } else {
            self.prompt_cache.update_with_bounds(
                self.scope.clone(),
                self.prompt_cache_profile.as_ref(),
                self.prompt_cache_target_read_ratio,
                self.prompt_cache_bounds,
            );
        }
    }
}
