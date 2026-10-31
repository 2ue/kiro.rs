use super::*;

#[derive(Debug, Default)]
struct ExternalUsageProjectionState {
    committed_controlled_usage: Option<CacheUsage>,
    observed_cache_read_evidence: bool,
    /// The channel reported a 1-hour cache write in any usage event of this response.
    observed_upstream_1h_write: bool,
    /// Write TTL resolved for the committed usage; sets the lifetime of the entries it writes.
    committed_write_ttl: Option<crate::anthropic::cache::CacheWriteTtl>,
}

/// Kiro-RS Tool Plus 策略在外部池上的整形参数。
#[derive(Debug, Clone, Copy)]
pub(super) struct ExternalKiroRsToolPlusProjection {
    pub(super) policy: crate::model::config::KiroRsToolPlusCachePolicy,
    pub(super) stream: bool,
    pub(super) raw_input_tokens: i32,
    pub(super) seed: u64,
}

impl ExternalKiroRsToolPlusProjection {
    pub(super) fn shaper(&self) -> crate::anthropic::kiro_plus_shaping::KiroPlusShaper {
        crate::anthropic::kiro_plus_shaping::KiroPlusShaper::new(
            self.policy,
            self.stream,
            self.raw_input_tokens,
            self.seed,
        )
    }
}

#[derive(Debug, Clone)]
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
    pub(super) stable_segment_prompt_cache_plan: Option<StableSegmentPromptCachePlan>,
    pub(super) stable_segment_cache_policy: Option<StableSegmentCachePolicy>,
    pub(super) prompt_cache_target_read_ratio: f64,
    pub(super) prompt_cache_bounds: PromptCacheBounds,
    pub(super) prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub(super) prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub(super) pricing_catalog: Arc<PricingCatalog>,
    pub(super) uplift_percent: u32,
    pub(super) cost_floor_enabled: bool,
    pub(super) cost_floor_margin_percent: u32,
    pub(super) output_uplift_min_tokens: i32,
    pub(super) output_uplift_percent: u32,
    pub(super) kiro_rs_tool_plus: Option<ExternalKiroRsToolPlusProjection>,
    /// `bounded_high_cache`：最终下游 usage 按上下限收口，收口之后不再改写。
    pub(super) bounded_final_caps: bool,
    pub(super) bounded_seed: u64,
    /// 路由 hitShaping 参数：收口时提供读取倍数上限和读写最小值。
    pub(super) cache_hit_shaping_policy: crate::model::config::CacheHitShapingPolicy,
    /// 外部池缓存写入时长（路由 `cacheTtl.externalPool`）与请求里的 `cache_control.ttl`，
    /// 在投影输出时结合渠道返回的拆分统一决定。
    pub(super) cache_ttl_mode: crate::model::config::CacheTtlMode,
    pub(super) request_cache_ttl: crate::anthropic::cache::RequestCacheTtl,
    state: Arc<SyncMutex<ExternalUsageProjectionState>>,
}

#[derive(Clone)]
pub(super) struct ExternalUsageProjectionTemplate {
    uplift_percent: u32,
    cost_floor_enabled: bool,
    cost_floor_margin_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
    raw_input_tokens: i32,
    cache_state_enabled: bool,
    model: String,
    simulated_usage: Option<CacheSimulation>,
    reported_policy: Option<ReportedCacheUsagePolicy>,
    scope: Option<PromptCacheScope>,
    prompt_cache: Arc<PromptCacheTracker>,
    prompt_cache_profile: Option<PromptCacheProfile>,
    kiro_rs_tool_prompt_cache_plan: Option<KiroRsToolPromptCachePlan>,
    stable_segment_prompt_cache_plan: Option<StableSegmentPromptCachePlan>,
    stable_segment_cache_policy: Option<StableSegmentCachePolicy>,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_bounds: PromptCacheBounds,
    prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pricing_catalog: Arc<PricingCatalog>,
    kiro_rs_tool_plus: Option<ExternalKiroRsToolPlusProjection>,
    bounded_final_caps: bool,
    cache_hit_shaping_policy: crate::model::config::CacheHitShapingPolicy,
    cache_ttl_mode: crate::model::config::CacheTtlMode,
    request_cache_ttl: crate::anthropic::cache::RequestCacheTtl,
}

impl ExternalUsageProjectionTemplate {
    fn matches_config(
        &self,
        uplift_percent: u32,
        cost_floor_enabled: bool,
        cost_floor_margin_percent: u32,
        output_uplift_min_tokens: i32,
        output_uplift_percent: u32,
    ) -> bool {
        self.uplift_percent == uplift_percent
            && self.cost_floor_enabled == cost_floor_enabled
            && self.cost_floor_margin_percent == cost_floor_margin_percent.min(200)
            && self.output_uplift_min_tokens == output_uplift_min_tokens.max(0)
            && self.output_uplift_percent == output_uplift_percent.min(200)
    }

    fn context_for_pool(&self, pool: &ExternalPool) -> ExternalUsageProjectionContext {
        let (scope, simulated_usage, stable_segment_prompt_cache_plan) =
            if let Some(policy) = self.stable_segment_cache_policy {
                let scope = self.scope.clone();
                let plan = self
                    .prompt_cache
                    .compute_stable_segment_with_profile_bounds(
                        scope.clone(),
                        self.prompt_cache_profile.clone(),
                        self.prompt_cache_bounds,
                        policy,
                    );
                let policy = policy.normalized();
                let simulated_usage =
                    CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(
                        plan.usage(),
                        policy.reported_input_min_tokens,
                        policy.reported_input_max_tokens,
                        plan.cache_jitter_seed(),
                    );
                (scope, simulated_usage, Some(plan))
            } else {
                (
                    self.scope.clone(),
                    self.simulated_usage,
                    self.stable_segment_prompt_cache_plan.clone(),
                )
            };

        ExternalUsageProjectionContext {
            mode: pool.usage_projection_mode,
            raw_input_tokens: self.raw_input_tokens,
            cache_state_enabled: self.cache_state_enabled,
            credential_key: Some(format!("external_pool:{}", pool.id)),
            model: self.model.clone(),
            simulated_usage,
            reported_policy: self.reported_policy.clone(),
            scope,
            prompt_cache: self.prompt_cache.clone(),
            prompt_cache_profile: self.prompt_cache_profile.clone(),
            kiro_rs_tool_prompt_cache_plan: self.kiro_rs_tool_prompt_cache_plan.clone(),
            stable_segment_prompt_cache_plan,
            stable_segment_cache_policy: self.stable_segment_cache_policy,
            prompt_cache_target_read_ratio: self.prompt_cache_target_read_ratio,
            prompt_cache_bounds: self.prompt_cache_bounds,
            prompt_cache_creation_controller: self.prompt_cache_creation_controller.clone(),
            prompt_cache_creation_control: self.prompt_cache_creation_control,
            pricing_catalog: self.pricing_catalog.clone(),
            uplift_percent: self.uplift_percent,
            cost_floor_enabled: self.cost_floor_enabled,
            cost_floor_margin_percent: self.cost_floor_margin_percent,
            output_uplift_min_tokens: self.output_uplift_min_tokens,
            output_uplift_percent: self.output_uplift_percent,
            kiro_rs_tool_plus: self.kiro_rs_tool_plus,
            bounded_final_caps: self.bounded_final_caps,
            bounded_seed: fastrand::u64(..),
            cache_hit_shaping_policy: self.cache_hit_shaping_policy,
            cache_ttl_mode: self.cache_ttl_mode,
            request_cache_ttl: self.request_cache_ttl,
            state: Arc::new(SyncMutex::new(ExternalUsageProjectionState::default())),
        }
    }
}

pub(super) fn build_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    cost_floor_enabled: bool,
    cost_floor_margin_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    if pool.usage_projection_mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }
    let cached = route
        .preparation_cache
        .usage_projection_template
        .get_or_init(|| {
            route.preparation_cache.record_usage_projection_build();
            build_template(
                route,
                uplift_percent,
                cost_floor_enabled,
                cost_floor_margin_percent,
                output_uplift_min_tokens,
                output_uplift_percent,
            )
            .map(Arc::new)
        });
    let template = match cached {
        Some(template)
            if template.matches_config(
                uplift_percent,
                cost_floor_enabled,
                cost_floor_margin_percent,
                output_uplift_min_tokens,
                output_uplift_percent,
            ) =>
        {
            template.clone()
        }
        Some(_) => Arc::new(build_template(
            route,
            uplift_percent,
            cost_floor_enabled,
            cost_floor_margin_percent,
            output_uplift_min_tokens,
            output_uplift_percent,
        )?),
        None => return None,
    };
    Some(template.context_for_pool(pool))
}

fn build_template(
    route: &ExternalRouteRequest,
    uplift_percent: u32,
    cost_floor_enabled: bool,
    cost_floor_margin_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionTemplate> {
    let stream = route.is_stream();
    let reported_usage = route.reported_usage.policy_for_path(&route.endpoint);
    if !stream && reported_usage.skip_non_stream_usage_projection {
        return None;
    }
    if !reported_usage.enabled
        && route
            .prompt_cache_strategy_type
            .uses_high_cache_simulation()
    {
        return None;
    }

    let raw_projection_payload;
    let payload = if let Some(payload) = route.payload() {
        payload
    } else {
        raw_projection_payload = external_route_raw_projection_payload(route)?;
        if raw_projection_payload.model.trim().is_empty() {
            return None;
        }
        raw_projection_payload.as_ref()
    };

    let model = route
        .upstream_model
        .clone()
        .unwrap_or_else(|| payload.model.clone());
    // Same rule as local requests: with the route's strict check on, a request the Anthropic API
    // could not return cache usage for (no cache breakpoint, or a prefix below the model minimum)
    // keeps the upstream usage; with it off, only a request shorter than the minimum does.
    if route.prompt_cache_strategy_type != PromptCacheStrategyType::NoCache
        && !crate::anthropic::prompt_cache::request_qualifies_for_prompt_cache(
            payload,
            !reported_usage.strict_cache_eligibility(stream)
                || route
                    .effective_raw_probe
                    .as_deref()
                    .is_some_and(|probe| probe.top_level_cache_control),
            &model,
        )
    {
        return None;
    }
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
    let (profile, kiro_rs_tool_prompt_cache_plan, stable_segment_cache_policy, simulated_usage) =
        match route.prompt_cache_strategy_type {
            PromptCacheStrategyType::CurrentHighCache
            | PromptCacheStrategyType::BoundedHighCache
                if prompt_cache_supported
                    && route.prompt_cache_simulation_mode
                        == PromptCacheSimulationMode::HighCache =>
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
                (profile, None, None, simulated_usage)
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
                (None, Some(plan), None, simulated_usage)
            }
            PromptCacheStrategyType::KiroRsToolPlus if prompt_cache_supported => {
                let plan = route.prompt_cache.compute_kiro_rs_tool_with_bounds(
                    scope.clone(),
                    payload,
                    raw_input_tokens,
                    &model,
                    route.prompt_cache_bounds,
                    route.kiro_rs_tool_plus_cache_policy.tracker_policy(),
                );
                let simulated_usage = CacheSimulation::from_prompt_cache_split_input(plan.usage());
                (None, Some(plan), None, simulated_usage)
            }
            PromptCacheStrategyType::StableSegmentCache if prompt_cache_supported => {
                let profile = route.prompt_cache.build_stable_segment_profile_for_model(
                    payload,
                    raw_input_tokens,
                    &model,
                    route.stable_segment_cache_policy,
                );
                (profile, None, Some(route.stable_segment_cache_policy), None)
            }
            _ => (None, None, None, None),
        };
    let reported_policy = match route.prompt_cache_strategy_type {
        PromptCacheStrategyType::NoCache => ReportedCacheUsagePolicy::from_path_policy(
            crate::model::config::ReportedUsagePathPolicy::disabled(),
            fastrand::u64(..),
        ),
        PromptCacheStrategyType::CurrentHighCache | PromptCacheStrategyType::BoundedHighCache
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
        PromptCacheStrategyType::KiroRsTool if prompt_cache_supported => reported_usage
            .enabled
            .then(|| ReportedCacheUsagePolicy::from_path_policy(reported_usage, fastrand::u64(..)))
            .flatten(),
        PromptCacheStrategyType::CurrentHighCache
        | PromptCacheStrategyType::BoundedHighCache
        | PromptCacheStrategyType::KiroRsTool
        | PromptCacheStrategyType::KiroRsToolPlus
        | PromptCacheStrategyType::StableSegmentCache => None,
    };
    let kiro_rs_tool_plus = (route.prompt_cache_strategy_type
        == PromptCacheStrategyType::KiroRsToolPlus)
        .then(|| ExternalKiroRsToolPlusProjection {
            policy: route.kiro_rs_tool_plus_cache_policy,
            stream,
            raw_input_tokens,
            seed: kiro_rs_tool_prompt_cache_plan
                .as_ref()
                .map(|plan| plan.cache_jitter_seed())
                .unwrap_or(0)
                ^ fastrand::u64(..),
        });
    Some(ExternalUsageProjectionTemplate {
        uplift_percent,
        cost_floor_enabled,
        cost_floor_margin_percent: cost_floor_margin_percent.min(200),
        output_uplift_min_tokens: output_uplift_min_tokens.max(0),
        output_uplift_percent: output_uplift_percent.min(200),
        raw_input_tokens,
        cache_state_enabled,
        model,
        simulated_usage,
        reported_policy,
        scope,
        prompt_cache: route.prompt_cache.clone(),
        prompt_cache_profile: profile,
        kiro_rs_tool_prompt_cache_plan,
        stable_segment_prompt_cache_plan: None,
        stable_segment_cache_policy,
        prompt_cache_target_read_ratio: route.prompt_cache_target_read_ratio,
        prompt_cache_bounds: route.prompt_cache_bounds,
        prompt_cache_creation_controller: route.prompt_cache_creation_controller.clone(),
        prompt_cache_creation_control: route.prompt_cache_creation_control,
        pricing_catalog: route.pricing_catalog.clone(),
        kiro_rs_tool_plus,
        bounded_final_caps: route.prompt_cache_strategy_type
            == PromptCacheStrategyType::BoundedHighCache,
        cache_hit_shaping_policy: route.cache_hit_shaping_policy,
        cache_ttl_mode: route.cache_ttl_mode,
        request_cache_ttl: crate::anthropic::cache::RequestCacheTtl::from_request(payload)
            .with_top_level(
                route
                    .effective_raw_probe
                    .as_deref()
                    .and_then(|probe| probe.top_level_cache_ttl),
            ),
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
    pub(super) fn observe_cache_read_evidence(&self, observed: bool) -> bool {
        let mut state = self.state.lock();
        state.observed_cache_read_evidence |= observed;
        state.observed_cache_read_evidence
    }

    pub(super) fn mark_committed(&self, usage: CacheUsage) {
        let mut state = self.state.lock();
        state.committed_controlled_usage = Some(usage);
    }

    /// Resolves the reported cache write TTL for this response. `upstream_usage` is the
    /// channel's own usage of the current event; once any event reports a 1-hour write the
    /// channel counts as having marked 1h for the rest of the response.
    pub(super) fn resolve_write_ttl(
        &self,
        upstream_usage: Option<&CacheUsage>,
        commit: bool,
    ) -> crate::anthropic::cache::CacheWriteTtl {
        let mut state = self.state.lock();
        state.observed_upstream_1h_write |=
            upstream_usage.is_some_and(|usage| usage.cache_creation_1h_input_tokens > 0);
        let ttl = crate::anthropic::cache::resolve_cache_write_ttl(
            self.cache_ttl_mode,
            self.request_cache_ttl,
            state.observed_upstream_1h_write,
        );
        if commit {
            state.committed_write_ttl = Some(ttl);
        }
        ttl
    }

    fn committed_bounds(&self) -> PromptCacheBounds {
        match self.state.lock().committed_write_ttl {
            Some(ttl) => self.prompt_cache_bounds.with_write_ttl(ttl),
            None => self.prompt_cache_bounds,
        }
    }

    pub(super) fn record_success(&self) {
        let Some(usage) = self.state.lock().committed_controlled_usage else {
            return;
        };
        if !self.cache_state_enabled {
            return;
        }
        let bounds = self.committed_bounds();
        if let Some(plus) = self.kiro_rs_tool_plus {
            if let Some(config) = plus.shaper().frequency_control() {
                let _ = self
                    .prompt_cache_creation_controller
                    .apply_success_with_mode(
                        self.scope.as_ref(),
                        config,
                        usage,
                        self.credential_key.as_deref(),
                        Some(self.model.as_str()),
                        true,
                    );
            }
        } else if self.reported_policy.is_some() {
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
                bounds,
            );
        } else if let Some(plan) = self.stable_segment_prompt_cache_plan.as_ref() {
            self.prompt_cache.commit_stable_segment_success_with_bounds(
                self.scope.clone(),
                plan,
                bounds,
                self.stable_segment_cache_policy.unwrap_or_default(),
            );
        } else {
            self.prompt_cache.update_with_bounds(
                self.scope.clone(),
                self.prompt_cache_profile.as_ref(),
                self.prompt_cache_target_read_ratio,
                bounds,
            );
        }
    }
}
