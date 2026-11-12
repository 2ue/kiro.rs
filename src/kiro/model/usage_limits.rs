//! 使用额度查询数据模型
//!
//! 包含 getUsageLimits API 的响应类型定义

use serde::Deserialize;

/// 使用额度查询响应
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsResponse {
    /// 上游账号身份信息。仅用于后台展示和补齐账号 email，不参与鉴权。
    #[serde(default)]
    pub user_info: Option<UserInfo>,

    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    pub next_date_reset: Option<f64>,

    /// 超额调用配置
    #[serde(default)]
    pub overage_configuration: Option<OverageConfiguration>,

    /// 订阅信息
    #[serde(default)]
    pub subscription_info: Option<SubscriptionInfo>,

    /// 使用量明细列表
    #[serde(default)]
    pub usage_breakdown_list: Vec<UsageBreakdown>,
}

/// 上游返回的账号身份信息。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    #[serde(default)]
    pub email: Option<String>,
    /// Portal `GetUserInfo` 返回的身份提供方（如 Google / Github / BuilderId）；REST 额度接口不返回。
    #[serde(default)]
    pub idp: Option<String>,
}

/// 订阅信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    /// 订阅标题 (KIRO PRO+ / KIRO FREE 等)
    #[serde(default)]
    pub subscription_title: Option<String>,

    /// 是否支持超额调用
    #[serde(default)]
    pub overage_capability: Option<String>,
}

/// 超额调用配置
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    /// ENABLED / DISABLED
    #[serde(default)]
    pub overage_status: Option<String>,
    /// 部分响应用 boolean 表示开关状态
    #[serde(default)]
    pub overage_enabled: Option<bool>,
}

/// 使用量明细
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct UsageBreakdown {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 奖励额度列表
    #[serde(default)]
    pub bonuses: Vec<Bonus>,

    /// 免费试用信息
    #[serde(default)]
    pub free_trial_info: Option<FreeTrialInfo>,

    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    pub next_date_reset: Option<f64>,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,

    /// 超额计费上限（美元）
    #[serde(default)]
    pub overage_cap: f64,

    /// 超额调用单价（美元）
    #[serde(default)]
    pub overage_rate: f64,

    /// 已产生的超额用量（积分）
    #[serde(default)]
    pub current_overages: f64,
}

/// 奖励额度
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: f64,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: f64,

    /// 状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub status: Option<String>,
}

impl Bonus {
    /// 本周期内仍算在账号额度里：激活中，或已用完（`EXHAUSTED`）。
    ///
    /// 用完的赠送积分仍是这个周期发放过、也确实用掉的额度。只认 `ACTIVE` 时，
    /// 赠送积分一用完，它的上限和已用会一起从总数里消失：Pro 账号用完前显示
    /// 745 + 500 = 1245，用完后却变成 745/745（59138 2026-11-09，上游原始响应
    /// `"status": "EXHAUSTED"`）。过期（`EXPIRED`）等其他状态不计入。
    pub fn counts_toward_period(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|s| s == "ACTIVE" || s == "EXHAUSTED")
    }
}

/// 免费试用信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct FreeTrialInfo {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 免费试用过期时间 (Unix 时间戳)
    #[serde(default)]
    pub free_trial_expiry: Option<f64>,

    /// 免费试用状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub free_trial_status: Option<String>,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

// ============ 便捷方法实现 ============

impl FreeTrialInfo {
    /// 检查免费试用是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.free_trial_status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl UsageLimitsResponse {
    /// 获取上游账号 email。
    pub fn email(&self) -> Option<&str> {
        self.user_info
            .as_ref()
            .and_then(|info| info.email.as_deref())
            .map(str::trim)
            .filter(|email| !email.is_empty())
    }

    /// 获取上游返回的身份提供方。
    pub fn idp(&self) -> Option<&str> {
        self.user_info
            .as_ref()
            .and_then(|info| info.idp.as_deref())
            .map(str::trim)
            .filter(|idp| !idp.is_empty())
    }

    /// 获取订阅标题
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.subscription_title.as_deref())
    }

    /// 获取超额能力。
    pub fn overage_capability(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.overage_capability.as_deref())
    }

    /// 获取超额开关状态。
    pub fn overage_status(&self) -> Option<String> {
        if let Some(status) = self
            .overage_configuration
            .as_ref()
            .and_then(|config| config.overage_status.as_deref())
            .map(|status| status.trim().to_ascii_uppercase())
            .filter(|status| !status.is_empty())
        {
            return Some(status);
        }
        self.overage_configuration
            .as_ref()
            .and_then(|config| config.overage_enabled)
            .map(|enabled| {
                if enabled {
                    "ENABLED".to_string()
                } else {
                    "DISABLED".to_string()
                }
            })
    }

    pub fn overage_cap(&self) -> f64 {
        self.primary_breakdown()
            .map(|breakdown| breakdown.overage_cap)
            .unwrap_or_default()
    }

    pub fn overage_rate(&self) -> f64 {
        self.primary_breakdown()
            .map(|breakdown| breakdown.overage_rate)
            .unwrap_or_default()
    }

    pub fn current_overages(&self) -> f64 {
        self.primary_breakdown()
            .map(|breakdown| breakdown.current_overages)
            .unwrap_or_default()
    }

    /// 获取第一个使用量明细
    fn primary_breakdown(&self) -> Option<&UsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    /// 获取总使用限额（精确值）
    ///
    /// 累加基础额度、激活的免费试用额度和本周期的奖励额度（激活中或已用完）
    pub fn usage_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.usage_limit_with_precision;

        // 累加激活的 free trial 额度
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.usage_limit_with_precision;
            }
        }

        // 累加本周期的 bonus 额度（激活中或已用完）
        for bonus in &breakdown.bonuses {
            if bonus.counts_toward_period() {
                total += bonus.usage_limit;
            }
        }

        total
    }

    /// 获取本周期的奖励额度总额（激活中或已用完）。
    pub fn active_bonus_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        breakdown
            .bonuses
            .iter()
            .filter(|bonus| bonus.counts_toward_period())
            .map(|bonus| bonus.usage_limit)
            .sum()
    }

    /// 获取总当前使用量（精确值）
    ///
    /// 累加基础使用量、激活的免费试用使用量和本周期的奖励使用量（激活中或已用完）
    pub fn current_usage(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.current_usage_with_precision;

        // 累加激活的 free trial 使用量
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.current_usage_with_precision;
            }
        }

        // 累加本周期的 bonus 使用量（激活中或已用完）
        for bonus in &breakdown.bonuses {
            if bonus.counts_toward_period() {
                total += bonus.current_usage;
            }
        }

        total
    }
}

#[cfg(test)]
mod tests {
    use super::UsageLimitsResponse;

    #[test]
    fn parses_overage_status_and_breakdown_fields() {
        let raw = r#"{
            "overageConfiguration": {"overageStatus": "ENABLED"},
            "subscriptionInfo": {
                "subscriptionTitle": "KIRO PRO",
                "overageCapability": "OVERAGE_CAPABLE"
            },
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 125.5,
                "usageLimitWithPrecision": 1000.0,
                "overageCap": 10.0,
                "overageRate": 0.04,
                "currentOverages": 0.12
            }]
        }"#;

        let parsed: UsageLimitsResponse = serde_json::from_str(raw).unwrap();

        assert_eq!(parsed.subscription_title(), Some("KIRO PRO"));
        assert_eq!(parsed.overage_capability(), Some("OVERAGE_CAPABLE"));
        assert_eq!(parsed.overage_status().as_deref(), Some("ENABLED"));
        assert_eq!(parsed.overage_cap(), 10.0);
        assert_eq!(parsed.overage_rate(), 0.04);
        assert_eq!(parsed.current_overages(), 0.12);
    }

    /// 59138 2026-11-09 #2677 的上游原始响应（字段裁剪）：Pro 订阅 745 + 赠送 500，
    /// 赠送积分已用完，状态为 EXHAUSTED。
    #[test]
    fn exhausted_bonus_still_counts_toward_limit_and_usage() {
        let raw = r#"{
            "subscriptionInfo": {"subscriptionTitle": "KIRO PRO"},
            "usageBreakdownList": [{
                "bonuses": [{
                    "bonusType": "FIRST_UPGRADE_TRIAL",
                    "currentUsage": 500.0,
                    "status": "EXHAUSTED",
                    "usageLimit": 500.0
                }],
                "currentUsageWithPrecision": 745.0,
                "usageLimitWithPrecision": 745.0
            }]
        }"#;
        let parsed: UsageLimitsResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.usage_limit(), 1_245.0);
        assert_eq!(parsed.current_usage(), 1_245.0);
        assert_eq!(parsed.active_bonus_limit(), 500.0);

        // 用完前：赠送积分 ACTIVE，同样计入。
        let active = raw
            .replace("\"EXHAUSTED\"", "\"ACTIVE\"")
            .replace("\"currentUsage\": 500.0", "\"currentUsage\": 0.0")
            .replace(
                "\"currentUsageWithPrecision\": 745.0",
                "\"currentUsageWithPrecision\": 69.0",
            );
        let parsed: UsageLimitsResponse = serde_json::from_str(&active).unwrap();
        assert_eq!(parsed.usage_limit(), 1_245.0);
        assert_eq!(parsed.current_usage(), 69.0);

        // 过期的赠送积分不再计入。
        let expired = raw.replace("\"EXHAUSTED\"", "\"EXPIRED\"");
        let parsed: UsageLimitsResponse = serde_json::from_str(&expired).unwrap();
        assert_eq!(parsed.usage_limit(), 745.0);
        assert_eq!(parsed.current_usage(), 745.0);
        assert_eq!(parsed.active_bonus_limit(), 0.0);
    }

    #[test]
    fn parses_boolean_overage_enabled_as_status() {
        let raw = r#"{"overageConfiguration": {"overageEnabled": false}}"#;
        let parsed: UsageLimitsResponse = serde_json::from_str(raw).unwrap();

        assert_eq!(parsed.overage_status().as_deref(), Some("DISABLED"));
    }
}
