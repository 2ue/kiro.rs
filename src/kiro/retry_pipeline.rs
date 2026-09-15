//! 本地账号 429 狂暴轮换策略
//!
//! 该模块只承载**纯策略计算**：给定配置与当前进度，回答"下一步该做什么"。
//! 所有 IO、账号获取、HTTP 发送仍留在 [`crate::kiro::provider`]，便于单测覆盖
//! 顺序语义而不需要起上游服务。
//!
//! # 设计基线
//!
//! 沿用 `../kiro-rs-main` 的 429 三分法骨架（风控型 429 / 带 `Retry-After` 的 429 /
//! 普通瞬态错误），狂暴模式只接在第三类之后：
//!
//! ```text
//! 429 到达
//!  ├─ ① 风控型 429        → 走既有风控路径，不狂暴
//!  ├─ ② 带 Retry-After    → 直接返回，遵守上游，不狂暴
//!  └─ ③ 普通 429/408/5xx
//!       ├─ 狂暴关闭 → 完全走既有逻辑（零影响）
//!       └─ 狂暴开启 → 优先换号 → 换 region → 换轮次
//! ```
//!
//! # 轮换顺序（优先换号）
//!
//! ```text
//! 轮次 1:
//!   region[0] : acct1 → acct2 → ... → acctN     ← 先把账号轮完
//!   region[1] : acct1 → acct2 → ... → acctN     ← 再换 region
//!        ↓ 一轮全失败，退避一次
//! 轮次 2:
//!   region[0] : acct1 → ...
//! ```
//!
//! 同一轮内切换账号**不退避**（不同账号不共享上游限流配额）；跨轮次退避，
//! 对齐 kiro-rs-main `retry_delay_throttle` 的 1s 基线，给账号配额留恢复窗口。

use std::time::Duration;

use crate::model::config::Config;

/// 狂暴轮数硬上限，与 admin 校验保持一致。
pub(crate) const MAX_BERSERK_ROUNDS: u32 = 10;

/// 单次请求的狂暴轮换策略快照。
///
/// 在请求开始时从 [`Config`] 读取一次，避免热重载在同一请求中途改变语义
/// 导致轮换计划前后不一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BerserkPlan {
    enabled: bool,
    /// 归一化后的 region 轮换列表；空表示不做 region 轮换。
    regions: Vec<String>,
    /// 完整遍历 `账号 × region` 的轮数，至少为 1。
    rounds: u32,
    /// 跨轮次退避。
    round_delay: Duration,
}

impl BerserkPlan {
    /// 从运行时配置构建策略快照。
    ///
    /// 对标 `LocalStreamRetryConfig::from_runtime_config`：所有越界值在这里
    /// 收敛，下游只消费已归一化的结果。
    pub(crate) fn from_config(config: &Config) -> Self {
        Self {
            enabled: config.local_berserk_mode_enabled,
            // 总开关关闭时直接丢弃 region 列表，使下游所有轮换判断统一退化为
            // "沿用凭据自身的 region"，即改造之前的端点行为。
            regions: if config.kiro_upstream_region_rotation_enabled {
                normalize_regions(&config.kiro_upstream_region_rotation)
            } else {
                Vec::new()
            },
            // 0 等价于 1 轮；上限与 admin 校验一致，防止配置文件绕过 admin 写入超大值。
            rounds: config.local_berserk_max_rounds.clamp(1, MAX_BERSERK_ROUNDS),
            round_delay: Duration::from_millis(config.local_berserk_round_delay_ms.min(60_000)),
        }
    }

    /// 狂暴模式是否生效。
    ///
    /// 关闭时调用方必须完整走既有重试逻辑，保证零影响。
    pub(crate) fn active(&self) -> bool {
        self.enabled
    }

    /// region 轮换是否生效。
    ///
    /// 该能力独立于狂暴模式：普通模式下配置了 region 列表也会在瞬态失败时
    /// 尝试其他 region。
    pub(crate) fn region_rotation_active(&self) -> bool {
        !self.regions.is_empty()
    }

    /// 轮换用的 region 列表。空列表表示沿用凭据自身解析出的 region。
    pub(crate) fn regions(&self) -> &[String] {
        &self.regions
    }

    /// 参与轮换的 region 数量，至少为 1（空列表表示"只用默认 region"这一种取值）。
    pub(crate) fn region_slots(&self) -> usize {
        self.regions.len().max(1)
    }

    pub(crate) fn rounds(&self) -> u32 {
        self.rounds
    }

    pub(crate) fn round_delay(&self) -> Duration {
        self.round_delay
    }

    /// 取第 `index` 个 region 覆盖值；空列表或越界时返回 `None`
    /// （表示不覆盖，沿用凭据自身的 `effective_api_region`）。
    pub(crate) fn region_at(&self, index: usize) -> Option<&str> {
        self.regions.get(index).map(String::as_str)
    }

    /// 狂暴模式下单次请求的尝试上限：`账号数 × region数 × 轮数`。
    ///
    /// 关闭时返回 `None`，调用方沿用既有的 `max_retry_attempts`。
    pub(crate) fn attempt_budget(&self, total_credentials: usize) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        let accounts = total_credentials.max(1);
        Some(
            accounts
                .saturating_mul(self.region_slots())
                .saturating_mul(self.rounds as usize)
                // 兜底硬上限：即使账号池异常膨胀也不至于把单个下游请求放大到失控。
                .min(2_000),
        )
    }
}

/// 归一化 region 列表：去空白、丢弃空项、保序去重。
///
/// 保序很重要——用户配置的第一个 region 是主力区域，轮换必须从它开始。
fn normalize_regions(configured: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::with_capacity(configured.len());
    for region in configured {
        let trimmed = region.trim();
        if trimmed.is_empty() {
            continue;
        }
        if normalized.iter().any(|existing| existing == trimmed) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

/// `账号 × region × 轮次` 的遍历游标。
///
/// 语义是"优先换号"：先在当前 region 上把所有账号试一遍，再切换 region，
/// 所有 region 都试完才进入下一轮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RotationCursor {
    region_index: usize,
    region_slots: usize,
    round: u32,
    max_rounds: u32,
}

/// 一轮账号耗尽后的下一步动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RotationStep {
    /// 切换到下一个 region，重新开放全部账号；同一轮内不退避。
    NextRegion { region_index: usize },
    /// 当前轮的所有 region 都试完，进入下一轮并退避。
    NextRound { round: u32, delay: Duration },
    /// 轮数耗尽，停止重试。
    Exhausted,
}

impl RotationCursor {
    pub(crate) fn new(plan: &BerserkPlan) -> Self {
        Self {
            region_index: 0,
            region_slots: plan.region_slots(),
            round: 1,
            max_rounds: plan.rounds(),
        }
    }

    pub(crate) fn region_index(&self) -> usize {
        self.region_index
    }

    pub(crate) fn round(&self) -> u32 {
        self.round
    }

    /// 普通模式下的 region 轮换：每次瞬态失败重试都换到下一个 region，
    /// 用完后回到第一个循环使用。
    ///
    /// 与 [`Self::advance_after_accounts_exhausted`] 的区别是这里没有"轮次"概念，
    /// 也不清空账号排除集合——普通模式的账号故障转移语义保持原样，
    /// 仅仅是让连续重试打到不同的上游地址。
    pub(crate) fn advance_region_for_normal_retry(&mut self) -> usize {
        if self.region_slots > 1 {
            self.region_index = (self.region_index + 1) % self.region_slots;
        }
        self.region_index
    }

    /// 当前 region 下所有账号都试过后调用，推进游标。
    ///
    /// 调用方拿到 [`RotationStep::NextRegion`] / [`RotationStep::NextRound`] 后
    /// 必须清空本请求的账号排除集合，让所有账号在新 region / 新轮次重新可选。
    pub(crate) fn advance_after_accounts_exhausted(&mut self, plan: &BerserkPlan) -> RotationStep {
        if self.region_index + 1 < self.region_slots {
            self.region_index += 1;
            return RotationStep::NextRegion {
                region_index: self.region_index,
            };
        }

        if self.round < self.max_rounds {
            self.round += 1;
            self.region_index = 0;
            return RotationStep::NextRound {
                round: self.round,
                delay: plan.round_delay(),
            };
        }

        RotationStep::Exhausted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(enabled: bool, regions: &[&str], rounds: u32, delay_ms: u64) -> Config {
        let mut config = Config::default();
        config.local_berserk_mode_enabled = enabled;
        config.kiro_upstream_region_rotation =
            regions.iter().map(|region| region.to_string()).collect();
        // 绝大多数用例关注轮换生效后的行为，默认把总开关打开；
        // 总开关本身的语义由专门的用例覆盖。
        config.kiro_upstream_region_rotation_enabled = !regions.is_empty();
        config.local_berserk_max_rounds = rounds;
        config.local_berserk_round_delay_ms = delay_ms;
        config
    }

    #[test]
    fn default_config_keeps_berserk_disabled_and_rotation_empty() {
        // 零影响的第一道防线：默认配置下策略必须完全不生效。
        let plan = BerserkPlan::from_config(&Config::default());
        assert!(!plan.active(), "狂暴模式默认必须关闭");
        assert!(!plan.region_rotation_active(), "region 轮换默认必须关闭");
        assert_eq!(
            plan.attempt_budget(20),
            None,
            "关闭时不得覆盖既有的 max_retry_attempts"
        );
    }

    #[test]
    fn region_rotation_can_be_enabled_without_berserk_mode() {
        // 用户明确要求：端点轮换可以加到普通模式去，独立于狂暴模式。
        let plan = BerserkPlan::from_config(&config_with(false, &["us-east-1", "eu-west-1"], 1, 0));
        assert!(!plan.active());
        assert!(plan.region_rotation_active());
        assert_eq!(plan.regions(), ["us-east-1", "eu-west-1"]);
    }

    #[test]
    fn region_rotation_switch_off_falls_back_to_the_original_endpoint() {
        // 用户诉求：某些 region 不可用导致调度一直失败时，关掉总开关即可一键回退，
        // 而不必清空已配置的 region 列表。
        let mut config = Config::default();
        config.kiro_upstream_region_rotation =
            vec!["us-east-1".to_string(), "eu-west-1".to_string()];
        config.kiro_upstream_region_rotation_enabled = false;

        let plan = BerserkPlan::from_config(&config);
        assert!(
            !plan.region_rotation_active(),
            "总开关关闭时必须视为未配置任何轮换 region"
        );
        assert_eq!(
            plan.region_at(0),
            None,
            "总开关关闭时必须不覆盖 region，即使用改造之前的端点"
        );
        assert_eq!(plan.region_slots(), 1, "总开关关闭时只有默认这一个槽位");

        let mut cursor = RotationCursor::new(&plan);
        assert_eq!(
            cursor.advance_region_for_normal_retry(),
            0,
            "总开关关闭时普通模式重试不得切换端点"
        );
    }

    #[test]
    fn region_rotation_switch_is_independent_of_berserk_mode() {
        // 两个开关必须正交：任意组合都不得互相影响。
        let mut config = Config::default();
        config.kiro_upstream_region_rotation =
            vec!["us-east-1".to_string(), "eu-west-1".to_string()];

        // 仅开端点轮换：普通模式下也能轮换端点。
        config.kiro_upstream_region_rotation_enabled = true;
        config.local_berserk_mode_enabled = false;
        let plan = BerserkPlan::from_config(&config);
        assert!(!plan.active(), "端点轮换不得连带开启狂暴模式");
        assert!(plan.region_rotation_active());

        // 仅开狂暴模式：账号轮换生效，但端点固定为改造之前的行为。
        config.kiro_upstream_region_rotation_enabled = false;
        config.local_berserk_mode_enabled = true;
        let plan = BerserkPlan::from_config(&config);
        assert!(plan.active(), "关闭端点轮换不得连带关闭狂暴模式");
        assert!(
            !plan.region_rotation_active(),
            "狂暴模式不得绕过端点轮换总开关"
        );
        assert_eq!(
            plan.attempt_budget(20),
            Some(20),
            "仅开狂暴模式时预算退化为 账号数 × 1个端点 × 1轮"
        );
    }

    #[test]
    fn normal_mode_region_rotation_cycles_through_every_region() {
        // 普通模式没有"轮次"概念，region 用完后回到第一个循环使用。
        let plan = BerserkPlan::from_config(&config_with(
            false,
            &["us-east-1", "eu-west-1", "ap-northeast-1"],
            1,
            0,
        ));
        let mut cursor = RotationCursor::new(&plan);
        assert_eq!(cursor.region_index(), 0, "首次请求必须使用主力 region");
        assert_eq!(cursor.advance_region_for_normal_retry(), 1);
        assert_eq!(cursor.advance_region_for_normal_retry(), 2);
        assert_eq!(
            cursor.advance_region_for_normal_retry(),
            0,
            "用完一圈后必须回到第一个 region 继续循环"
        );
    }

    #[test]
    fn normal_mode_region_rotation_is_a_noop_without_configured_regions() {
        // 零影响保证：没有配置 region 列表时，游标必须恒定停在默认槽位。
        let plan = BerserkPlan::from_config(&Config::default());
        let mut cursor = RotationCursor::new(&plan);
        for _ in 0..5 {
            assert_eq!(
                cursor.advance_region_for_normal_retry(),
                0,
                "未配置 region 轮换时不得产生任何 region 覆盖"
            );
        }
        assert_eq!(plan.region_at(0), None, "槽位 0 必须表示\"不覆盖\"");
    }

    #[test]
    fn regions_are_trimmed_deduplicated_and_order_preserved() {
        let plan = BerserkPlan::from_config(&config_with(
            true,
            &["  us-east-1  ", "", "eu-west-1", "us-east-1", "   "],
            1,
            0,
        ));
        assert_eq!(
            plan.regions(),
            ["us-east-1", "eu-west-1"],
            "必须去空白、丢空项、保序去重，且首个 region 保持为主力区域"
        );
    }

    #[test]
    fn rounds_are_clamped_into_the_supported_range() {
        // 0 等价 1 轮；超过上限收敛到 10，防止绕过 admin 直接写配置文件放大请求量。
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, &[], 0, 0)).rounds(),
            1
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, &[], 1, 0)).rounds(),
            1
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, &[], 10, 0)).rounds(),
            10
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, &[], 9_999, 0)).rounds(),
            MAX_BERSERK_ROUNDS
        );
    }

    #[test]
    fn round_delay_is_capped_at_one_minute() {
        let plan = BerserkPlan::from_config(&config_with(true, &[], 1, 10_000_000));
        assert_eq!(plan.round_delay(), Duration::from_millis(60_000));
    }

    #[test]
    fn attempt_budget_is_accounts_times_regions_times_rounds() {
        let plan = BerserkPlan::from_config(&config_with(
            true,
            &["us-east-1", "eu-west-1", "ap-northeast-1"],
            2,
            0,
        ));
        // 20 账号 × 3 region × 2 轮
        assert_eq!(plan.attempt_budget(20), Some(120));
    }

    #[test]
    fn attempt_budget_without_regions_degrades_to_accounts_times_rounds() {
        // 空 region 列表 = 只换号不换 region，region_slots 记为 1。
        let plan = BerserkPlan::from_config(&config_with(true, &[], 3, 0));
        assert_eq!(plan.attempt_budget(20), Some(60));
    }

    #[test]
    fn attempt_budget_treats_empty_pool_as_a_single_account() {
        let plan = BerserkPlan::from_config(&config_with(true, &["us-east-1"], 2, 0));
        assert_eq!(plan.attempt_budget(0), Some(2));
    }

    #[test]
    fn attempt_budget_is_hard_capped_against_runaway_pools() {
        let plan = BerserkPlan::from_config(&config_with(true, &["a", "b", "c", "d"], 10, 0));
        assert_eq!(
            plan.attempt_budget(100_000),
            Some(2_000),
            "即使账号池异常膨胀，单请求放大也必须封顶"
        );
    }

    #[test]
    fn cursor_prefers_switching_accounts_then_region_then_round() {
        // 这是用户要求的核心顺序语义：优先换号 → 换 region → 换轮次。
        let plan =
            BerserkPlan::from_config(&config_with(true, &["us-east-1", "eu-west-1"], 2, 1_000));
        let mut cursor = RotationCursor::new(&plan);

        assert_eq!(cursor.region_index(), 0);
        assert_eq!(cursor.round(), 1);

        // region[0] 上账号耗尽 → 换到 region[1]，同一轮内不退避
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::NextRegion { region_index: 1 }
        );

        // region[1] 也耗尽 → 进入第 2 轮，回到 region[0]，并退避
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::NextRound {
                round: 2,
                delay: Duration::from_millis(1_000),
            }
        );
        assert_eq!(cursor.region_index(), 0, "新一轮必须从主力 region 重新开始");

        // 第 2 轮重复同样的过程
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::NextRegion { region_index: 1 }
        );

        // 轮数耗尽
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::Exhausted
        );
    }

    #[test]
    fn cursor_without_regions_only_advances_rounds() {
        let plan = BerserkPlan::from_config(&config_with(true, &[], 2, 500));
        let mut cursor = RotationCursor::new(&plan);

        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::NextRound {
                round: 2,
                delay: Duration::from_millis(500),
            },
            "没有 region 列表时应直接进入下一轮，不产生 NextRegion"
        );
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::Exhausted
        );
    }

    #[test]
    fn single_round_single_region_exhausts_immediately() {
        let plan = BerserkPlan::from_config(&config_with(true, &[], 1, 0));
        let mut cursor = RotationCursor::new(&plan);
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::Exhausted,
            "1 轮 + 无 region 轮换时，账号试完即结束"
        );
    }

    #[test]
    fn region_at_returns_none_when_rotation_is_disabled() {
        let plan = BerserkPlan::from_config(&config_with(true, &[], 1, 0));
        assert_eq!(
            plan.region_at(0),
            None,
            "无 region 列表时必须返回 None，表示沿用凭据自身解析出的 region"
        );
    }

    #[test]
    fn region_at_walks_the_configured_list_in_order() {
        let plan = BerserkPlan::from_config(&config_with(true, &["us-east-1", "eu-west-1"], 1, 0));
        assert_eq!(plan.region_at(0), Some("us-east-1"));
        assert_eq!(plan.region_at(1), Some("eu-west-1"));
        assert_eq!(plan.region_at(2), None);
    }

    #[test]
    fn full_rotation_sequence_matches_the_documented_order() {
        // 端到端复现文档里的顺序图：2 region × 2 轮，每轮先把账号轮完。
        let plan = BerserkPlan::from_config(&config_with(true, &["r0", "r1"], 2, 0));
        let mut cursor = RotationCursor::new(&plan);
        let mut visited = vec![(cursor.round(), cursor.region_index())];

        loop {
            match cursor.advance_after_accounts_exhausted(&plan) {
                RotationStep::NextRegion { .. } | RotationStep::NextRound { .. } => {
                    visited.push((cursor.round(), cursor.region_index()));
                }
                RotationStep::Exhausted => break,
            }
        }

        assert_eq!(
            visited,
            vec![(1, 0), (1, 1), (2, 0), (2, 1)],
            "必须是 轮次1-region0 → 轮次1-region1 → 轮次2-region0 → 轮次2-region1"
        );
    }
}
