//! 账号劣化分：按模型累积「与账号相关的可疑失败」，达到阈值后暂时移出调度候选。
//!
//! 背景（2026-11-04 生产 59137）：上游返回 200 但没有任何正文（空轮次、零帧、空 body）时，
//! 账号按设计不冷却。于是一个持续劣化的账号会长期占住它所在 Fast 档位的全部流量——
//! 它从不「满载」（失败返回极快，并发锁立即释放），所以档位筛选永远不会让位给下一档。
//!
//! 这里给这类失败一个按模型累积的分数：
//!
//! - 失败加权累加（空轮次比流截断更指向账号），成功按系数衰减；
//! - 读取时按半衰期做惰性时间衰减，不引入定时器或后台任务；
//! - 分数达到阈值后在调度里暂时摘除，但**没有健康候选时仍全部放行**，
//!   保证摘除不会让请求排队；
//! - 少量探测流量继续打给被摘除的账号，让它能用成功样本证明自己恢复。
//!
//! 分数只存在进程内、以模型名为键，最多为每个账号保留
//! [`MAX_MODELS_PER_CREDENTIAL`] 个模型，避免异常模型名让内存无限增长。

use std::collections::HashMap;

/// 每个账号最多保留的模型条数。超出时丢弃衰减后分数最低的一项。
pub(super) const MAX_MODELS_PER_CREDENTIAL: usize = 8;

/// 衰减到这个分数以下时直接删除，避免留下无意义的空条目。
const SCORE_EPSILON: f64 = 0.01;

#[derive(Debug, Clone, Copy)]
pub(super) struct DegradeScore {
    score: f64,
    updated_at_ms: i64,
    /// 是否已被摘除。摘除需要「分数 ≥ 阈值」，恢复需要「分数 < 恢复线」，
    /// 两条线之间的滞回带防止账号在恢复/摘除之间高频横跳。
    suspended: bool,
}

impl DegradeScore {
    /// 按半衰期做时间衰减后的当前分数。
    fn effective(&self, now_ms: i64, half_life_ms: f64) -> f64 {
        let elapsed_ms = now_ms.saturating_sub(self.updated_at_ms).max(0) as f64;
        let half_life_ms = half_life_ms.max(1.0);
        self.score * 0.5f64.powf(elapsed_ms / half_life_ms)
    }
}

/// 模型名归一化；`None`（未指定模型）用空串作为键。
pub(super) fn model_key(model: Option<&str>) -> String {
    model.map(|value| value.trim().to_ascii_lowercase()).unwrap_or_default()
}

/// 该账号在 `model` 上是否应被摘除。
///
/// 进入条件是分数达到 `suspend_score`；退出条件不是同一个值，而是更低的
/// `resume_score`：一次成功只把分数减半，如果恢复线等于摘除线，账号会立刻回来、
/// 再立刻被摘掉，流量在档位之间来回横跳。
pub(super) fn is_suspended(
    scores: &HashMap<String, DegradeScore>,
    model: Option<&str>,
    now_ms: i64,
    half_life_ms: f64,
    resume_score: f64,
) -> bool {
    let Some(score) = scores.get(&model_key(model)) else {
        return false;
    };
    score.suspended && score.effective(now_ms, half_life_ms) >= resume_score
}

/// 该账号在 `model` 上当前的劣化分。
pub(super) fn effective_score(
    scores: &HashMap<String, DegradeScore>,
    model: Option<&str>,
    now_ms: i64,
    half_life_ms: f64,
) -> f64 {
    scores
        .get(&model_key(model))
        .map_or(0.0, |score| score.effective(now_ms, half_life_ms))
}

/// 记一次可疑失败。`weight` 为 0 或负数时忽略。
pub(super) fn add_failure(
    scores: &mut HashMap<String, DegradeScore>,
    model: Option<&str>,
    weight: f64,
    now_ms: i64,
    half_life_ms: f64,
    suspend_score: f64,
) {
    if !(weight > 0.0) {
        return;
    }
    let key = model_key(model);
    let current = scores
        .get(&key)
        .map_or(0.0, |score| score.effective(now_ms, half_life_ms));
    if !scores.contains_key(&key) && scores.len() >= MAX_MODELS_PER_CREDENTIAL {
        evict_lowest(scores, now_ms, half_life_ms);
    }
    let next = current + weight;
    let was_suspended = scores.get(&key).is_some_and(|score| score.suspended);
    scores.insert(
        key,
        DegradeScore {
            score: next,
            updated_at_ms: now_ms,
            suspended: was_suspended || (suspend_score > 0.0 && next >= suspend_score),
        },
    );
}

/// 成功一次：先做时间衰减，再乘 `decay`（取值 0~1）。衰减到阈值以下直接删除。
pub(super) fn apply_success(
    scores: &mut HashMap<String, DegradeScore>,
    model: Option<&str>,
    decay: f64,
    now_ms: i64,
    half_life_ms: f64,
    resume_score: f64,
) {
    let key = model_key(model);
    let Some(existing) = scores.get(&key).copied() else {
        return;
    };
    let next = existing.effective(now_ms, half_life_ms) * decay.clamp(0.0, 1.0);
    if next <= SCORE_EPSILON {
        scores.remove(&key);
        return;
    }
    if let Some(slot) = scores.get_mut(&key) {
        *slot = DegradeScore {
            score: next,
            updated_at_ms: now_ms,
            // 只有降到恢复线以下才解除摘除
            suspended: existing.suspended && next >= resume_score,
        };
    }
}

fn evict_lowest(
    scores: &mut HashMap<String, DegradeScore>,
    now_ms: i64,
    half_life_ms: f64,
) {
    let mut lowest: Option<(String, f64)> = None;
    for (key, score) in scores.iter() {
        let effective = score.effective(now_ms, half_life_ms);
        if lowest
            .as_ref()
            .is_none_or(|(_, lowest_score)| effective < *lowest_score)
        {
            lowest = Some((key.clone(), effective));
        }
    }
    if let Some((key, _)) = lowest {
        scores.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF_LIFE_MS: f64 = 120_000.0;

    fn scores() -> HashMap<String, DegradeScore> {
        HashMap::new()
    }

    #[test]
    fn failures_accumulate_and_time_decay_halves_the_score() {
        let mut map = scores();
        add_failure(&mut map, Some("opus"), 2.0, 0, HALF_LIFE_MS, 6.0);
        add_failure(&mut map, Some("opus"), 2.0, 0, HALF_LIFE_MS, 6.0);
        assert_eq!(effective_score(&map, Some("opus"), 0, HALF_LIFE_MS), 4.0);
        // 半个半衰期后剩约 70.7%
        let after = effective_score(&map, Some("opus"), 60_000, HALF_LIFE_MS);
        assert!((after - 4.0 * 0.5f64.powf(0.5)).abs() < 1e-9);
        // 两个半衰期后剩 25%
        let after = effective_score(&map, Some("opus"), 240_000, HALF_LIFE_MS);
        assert!((after - 1.0).abs() < 1e-9);
    }

    #[test]
    fn success_decays_and_then_removes_the_entry() {
        let mut map = scores();
        add_failure(&mut map, Some("opus"), 6.0, 0, HALF_LIFE_MS, 6.0);
        apply_success(&mut map, Some("opus"), 0.5, 0, HALF_LIFE_MS, 2.4);
        assert_eq!(effective_score(&map, Some("opus"), 0, HALF_LIFE_MS), 3.0);
        apply_success(&mut map, Some("opus"), 0.5, 0, HALF_LIFE_MS, 2.4);
        assert_eq!(effective_score(&map, Some("opus"), 0, HALF_LIFE_MS), 1.5);
        // 连续成功后条目被回收，内存不会长期增长
        for _ in 0..20 {
            apply_success(&mut map, Some("opus"), 0.5, 0, HALF_LIFE_MS, 2.4);
        }
        assert!(map.is_empty());
    }

    #[test]
    fn models_are_isolated() {
        let mut map = scores();
        add_failure(&mut map, Some("opus"), 6.0, 0, HALF_LIFE_MS, 6.0);
        assert_eq!(effective_score(&map, Some("sonnet"), 0, HALF_LIFE_MS), 0.0);
    }

    #[test]
    fn model_map_is_bounded() {
        let mut map = scores();
        for index in 0..(MAX_MODELS_PER_CREDENTIAL + 5) {
            add_failure(&mut map, Some(&format!("model-{index}")), 1.0, 0, HALF_LIFE_MS, 6.0);
        }
        assert_eq!(map.len(), MAX_MODELS_PER_CREDENTIAL);
    }

    #[test]
    fn eviction_keeps_the_worst_model() {
        let mut map = scores();
        add_failure(&mut map, Some("bad"), 9.0, 0, HALF_LIFE_MS, 6.0);
        for index in 0..MAX_MODELS_PER_CREDENTIAL {
            add_failure(&mut map, Some(&format!("ok-{index}")), 0.1, 0, HALF_LIFE_MS, 6.0);
        }
        assert!(effective_score(&map, Some("bad"), 0, HALF_LIFE_MS) > 0.0);
    }
}
