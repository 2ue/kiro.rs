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

use crate::model::config::{Config, MAX_LOCAL_BERSERK_ROUNDS};

/// 狂暴轮数硬上限，与 admin 校验共用 `MAX_LOCAL_BERSERK_ROUNDS` 这一个来源。
pub(crate) const MAX_BERSERK_ROUNDS: u32 = MAX_LOCAL_BERSERK_ROUNDS;

/// 官方 Kiro/Q 上游只在 `us-east-1` 与 `eu-central-1` 两个端点提供服务。
///
/// 取自 `../kiro-rs-main` `rest_api_region_candidates`（`src/kiro/token_manager.rs:465`）：
/// 该项目对 getUsageLimits / ListAvailableModels 就是在这两个端点间做 403 回退。
/// 因此端点轮换不需要用户填写具体 region——填了也只有这两个可用，填错反而
/// 会把请求打到不存在的域名上。
pub(crate) const KIRO_ROTATION_REGIONS: [&str; 2] = ["us-east-1", "eu-central-1"];

/// 按凭据自身的 region 决定轮换顺序，返回另一个端点作为回退候选。
///
/// 与 kiro-rs-main 的规则一致：`eu-central-1` 或任意 `eu-*` 账号以
/// `eu-central-1` 为主端点，其余以 `us-east-1` 为主端点。这样 Enterprise / IdC
/// 账号即使 SSO 区域不是 `us-east-1` 也能先命中正确的端点。
pub(crate) fn rotation_regions_for(credential_region: &str) -> [&'static str; 2] {
    let region = credential_region.trim();
    if region == "eu-central-1" || region.starts_with("eu-") {
        ["eu-central-1", "us-east-1"]
    } else {
        ["us-east-1", "eu-central-1"]
    }
}

/// 单次请求的狂暴轮换策略快照。
///
/// 在请求开始时从 [`Config`] 读取一次，避免热重载在同一请求中途改变语义
/// 导致轮换计划前后不一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BerserkPlan {
    enabled: bool,
    /// 端点轮换总开关；关闭时沿用凭据自身解析出的 region。
    region_rotation_enabled: bool,
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
            // 端点固定为官方支持的两个（对齐 kiro-rs-main），用户只需要开关。
            // 关闭时置空，使下游所有轮换判断统一退化为"沿用凭据自身的 region"，
            // 即改造之前的端点行为。
            region_rotation_enabled: config.kiro_upstream_region_rotation_enabled,
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
    /// 该能力独立于狂暴模式：普通模式下开启也会在瞬态失败时尝试另一个端点。
    pub(crate) fn region_rotation_active(&self) -> bool {
        self.region_rotation_enabled
    }

    /// 参与轮换的 region 数量。
    ///
    /// 关闭时为 1（"只用凭据自身的 region"这一种取值），开启时为官方支持的两个端点。
    pub(crate) fn region_slots(&self) -> usize {
        if self.region_rotation_enabled {
            KIRO_ROTATION_REGIONS.len()
        } else {
            1
        }
    }

    pub(crate) fn rounds(&self) -> u32 {
        self.rounds
    }

    pub(crate) fn round_delay(&self) -> Duration {
        self.round_delay
    }

    /// 取第 `index` 个 region 覆盖值，顺序由凭据自身的 region 决定
    /// （对齐 kiro-rs-main：`eu-*` 账号优先 `eu-central-1`）。
    ///
    /// 关闭轮换或越界时返回 `None`，表示不覆盖、沿用凭据自身的
    /// `effective_api_region`，即改造之前的端点。
    pub(crate) fn region_at(&self, credential_region: &str, index: usize) -> Option<&'static str> {
        if !self.region_rotation_enabled {
            return None;
        }
        rotation_regions_for(credential_region).get(index).copied()
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

    fn config_with(berserk: bool, rotation: bool, rounds: u32, delay_ms: u64) -> Config {
        let mut config = Config::default();
        config.local_berserk_mode_enabled = berserk;
        config.kiro_upstream_region_rotation_enabled = rotation;
        config.local_berserk_max_rounds = rounds;
        config.local_berserk_round_delay_ms = delay_ms;
        config
    }

    #[test]
    fn default_config_keeps_both_switches_disabled() {
        // 零影响的第一道防线：默认配置下两个开关都必须不生效。
        let plan = BerserkPlan::from_config(&Config::default());
        assert!(!plan.active(), "狂暴模式默认必须关闭");
        assert!(!plan.region_rotation_active(), "端点轮换默认必须关闭");
        assert_eq!(
            plan.region_slots(),
            1,
            "关闭时只有「凭据自身 region」一个槽位"
        );
        assert_eq!(
            plan.attempt_budget(20),
            None,
            "关闭时不得覆盖既有的 max_retry_attempts"
        );
    }

    #[test]
    fn rotation_endpoints_follow_kiro_rs_main_ordering() {
        // 对齐 kiro-rs-main rest_api_region_candidates：eu-* 账号优先 eu-central-1，
        // 其余优先 us-east-1，另一个作为回退候选。
        assert_eq!(
            rotation_regions_for("us-east-1"),
            ["us-east-1", "eu-central-1"]
        );
        assert_eq!(
            rotation_regions_for("ap-northeast-1"),
            ["us-east-1", "eu-central-1"],
            "非 eu 账号一律以 us-east-1 为主端点"
        );
        assert_eq!(
            rotation_regions_for("eu-central-1"),
            ["eu-central-1", "us-east-1"]
        );
        assert_eq!(
            rotation_regions_for("eu-west-1"),
            ["eu-central-1", "us-east-1"],
            "任意 eu-* 账号都应优先命中 eu-central-1，避免 403 Invalid token"
        );
        assert_eq!(
            rotation_regions_for("  eu-west-1  "),
            ["eu-central-1", "us-east-1"],
            "region 两侧空白不得影响端点选择"
        );
    }

    #[test]
    fn region_rotation_switch_off_falls_back_to_the_original_endpoint() {
        // 用户诉求：某个端点不可用导致调度一直失败时，关掉总开关即可一键回退。
        let plan = BerserkPlan::from_config(&config_with(true, false, 1, 0));
        assert!(!plan.region_rotation_active());
        assert_eq!(
            plan.region_at("us-east-1", 0),
            None,
            "关闭时必须不覆盖 region，即使用改造之前的端点"
        );
        assert_eq!(plan.region_slots(), 1);

        let mut cursor = RotationCursor::new(&plan);
        assert_eq!(
            cursor.advance_region_for_normal_retry(),
            0,
            "关闭时普通模式重试不得切换端点"
        );
    }

    #[test]
    fn region_rotation_switch_on_exposes_both_official_endpoints() {
        let plan = BerserkPlan::from_config(&config_with(false, true, 1, 0));
        assert!(plan.region_rotation_active());
        assert_eq!(plan.region_slots(), 2, "官方只有两个端点提供服务");
        assert_eq!(plan.region_at("us-east-1", 0), Some("us-east-1"));
        assert_eq!(plan.region_at("us-east-1", 1), Some("eu-central-1"));
        assert_eq!(
            plan.region_at("us-east-1", 2),
            None,
            "越界必须返回 None 而不是 panic"
        );
    }

    #[test]
    fn the_two_switches_are_fully_orthogonal() {
        // 仅开端点轮换：普通模式下也能轮换端点，且不得连带开启狂暴。
        let plan = BerserkPlan::from_config(&config_with(false, true, 1, 0));
        assert!(!plan.active(), "端点轮换不得连带开启狂暴模式");
        assert!(plan.region_rotation_active());
        assert_eq!(plan.attempt_budget(20), None, "未开狂暴不得覆盖尝试预算");

        // 仅开狂暴模式：账号轮换生效，但端点固定为改造之前的行为。
        let plan = BerserkPlan::from_config(&config_with(true, false, 1, 0));
        assert!(plan.active(), "关闭端点轮换不得连带关闭狂暴模式");
        assert!(
            !plan.region_rotation_active(),
            "狂暴模式不得绕过端点轮换总开关"
        );
        assert_eq!(
            plan.attempt_budget(20),
            Some(20),
            "仅开狂暴时预算退化为 账号数 × 1个端点 × 1轮"
        );

        // 同时开：完整笛卡尔积。
        let plan = BerserkPlan::from_config(&config_with(true, true, 3, 0));
        assert_eq!(
            plan.attempt_budget(20),
            Some(20 * 2 * 3),
            "同时开启时预算为 账号数 × 端点数 × 轮数"
        );
    }

    #[test]
    fn normal_mode_region_rotation_cycles_between_both_endpoints() {
        // 普通模式没有"轮次"概念，端点用完后回到第一个循环使用。
        let plan = BerserkPlan::from_config(&config_with(false, true, 1, 0));
        let mut cursor = RotationCursor::new(&plan);
        assert_eq!(cursor.region_index(), 0, "首次请求必须使用主力端点");
        assert_eq!(cursor.advance_region_for_normal_retry(), 1);
        assert_eq!(
            cursor.advance_region_for_normal_retry(),
            0,
            "用完一圈后必须回到第一个端点继续循环"
        );
    }

    #[test]
    fn rounds_are_clamped_into_the_supported_range() {
        // 0 等价 1 轮；超过上限收敛到 10，防止绕过 admin 直接写配置文件放大请求量。
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, false, 0, 0)).rounds(),
            1
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, false, 1, 0)).rounds(),
            1
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, false, 10, 0)).rounds(),
            10
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, false, 999, 0)).rounds(),
            MAX_BERSERK_ROUNDS
        );
    }

    #[test]
    fn round_delay_is_capped_at_one_minute() {
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, false, 1, 999_999)).round_delay(),
            Duration::from_millis(60_000)
        );
        assert_eq!(
            BerserkPlan::from_config(&config_with(true, false, 1, 0)).round_delay(),
            Duration::ZERO
        );
    }

    #[test]
    fn cursor_prefers_switching_accounts_then_region_then_round() {
        // 用户明确要求的顺序：优先换号 → 换端点 → 换轮次。
        let plan = BerserkPlan::from_config(&config_with(true, true, 2, 1_000));
        let mut cursor = RotationCursor::new(&plan);

        // 端点[0] 账号耗尽 → 换到端点[1]，同轮内不退避。
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::NextRegion { region_index: 1 }
        );
        // 端点[1] 也耗尽 → 进入第 2 轮并退避。
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::NextRound {
                round: 2,
                delay: Duration::from_millis(1_000)
            }
        );
        assert_eq!(cursor.region_index(), 0, "新一轮必须从主力端点重新开始");
    }

    #[test]
    fn full_rotation_sequence_matches_the_documented_order() {
        let plan = BerserkPlan::from_config(&config_with(true, true, 2, 0));
        let mut cursor = RotationCursor::new(&plan);
        let mut visited = vec![(cursor.round(), cursor.region_index())];
        while !matches!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::Exhausted
        ) {
            visited.push((cursor.round(), cursor.region_index()));
        }
        assert_eq!(
            visited,
            vec![(1, 0), (1, 1), (2, 0), (2, 1)],
            "必须先把端点轮完再进下一轮"
        );
    }

    #[test]
    fn cursor_without_rotation_only_advances_rounds() {
        let plan = BerserkPlan::from_config(&config_with(true, false, 3, 0));
        let mut cursor = RotationCursor::new(&plan);
        for expected_round in 2..=3 {
            assert_eq!(
                cursor.advance_after_accounts_exhausted(&plan),
                RotationStep::NextRound {
                    round: expected_round,
                    delay: Duration::ZERO
                }
            );
            assert_eq!(cursor.region_index(), 0, "未开轮换时端点必须恒为默认槽位");
        }
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::Exhausted
        );
    }

    #[test]
    fn single_round_single_region_exhausts_immediately() {
        let plan = BerserkPlan::from_config(&config_with(true, false, 1, 0));
        let mut cursor = RotationCursor::new(&plan);
        assert_eq!(
            cursor.advance_after_accounts_exhausted(&plan),
            RotationStep::Exhausted,
            "1 轮 + 不轮换端点时，账号耗尽即结束"
        );
    }

    #[test]
    fn attempt_budget_is_hard_capped_against_runaway_pools() {
        let plan = BerserkPlan::from_config(&config_with(true, true, 10, 0));
        assert_eq!(
            plan.attempt_budget(100_000),
            Some(2_000),
            "账号池异常膨胀时必须命中兜底硬上限"
        );
    }

    #[test]
    fn attempt_budget_treats_empty_pool_as_a_single_account() {
        let plan = BerserkPlan::from_config(&config_with(true, true, 2, 0));
        // 账号 1（空池按单账号兜底）× 端点 2 × 轮数 2。
        assert_eq!(plan.attempt_budget(0), Some(4));
    }
}
