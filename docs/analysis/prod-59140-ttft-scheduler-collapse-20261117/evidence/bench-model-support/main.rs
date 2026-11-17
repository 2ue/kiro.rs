#![allow(dead_code)]
mod new;
mod old;
use std::time::Instant;

fn main() {
    let free: Vec<String> = ["auto", "claude-3-5-haiku-20241022", "claude-3-5-sonnet-20240620", "claude-3-5-sonnet-20241022", "claude-3-7-sonnet-20250219", "claude-fable-5", "claude-fable-5-1", "claude-fable-5-2", "claude-haiku-4-5", "claude-haiku-4-5-20251001", "claude-haiku-4.5", "claude-haiku-5-5", "claude-opus-4-1-20250805", "claude-opus-4-20250514", "claude-opus-4-5", "claude-opus-4-5-20251101", "claude-opus-4-6", "claude-opus-4-7", "claude-opus-4-8", "claude-opus-5", "claude-opus-5-5", "claude-sonnet-4-20250514", "claude-sonnet-4-5", "claude-sonnet-4-5-20250929", "claude-sonnet-4-6", "claude-sonnet-5", "claude-sonnet-5-5", "deepseek-3.2", "minimax-m2.5", "minimax-m2.1", "glm-5", "qwen3-coder-next", "claude-sonnet-4.5"].iter().map(|s| s.to_string()).collect();
    let accounts = 848;
    let candidates = ["claude-haiku-4.5", "claude-opus-5", "claude-sonnet-4-6[1m]", "gpt-4o"];
    // 正确性：新旧实现逐一比对
    for c in candidates {
        for _ in 0..3 {
            assert_eq!(new::model_is_supported_by_list(&free, &[Some(c)]), old::model_is_supported_by_list(&free, &[Some(c)]), "{c}");
        }
    }
    for round in 0..3 {
        let t = Instant::now();
        let mut h = 0usize;
        for _ in 0..accounts { if old::model_is_supported_by_list(&free, &[Some("claude-haiku-4.5")]) { h += 1; } if old::subscription_tier_key(Some("KIRO FREE")) == "free" { h += 1; } }
        let old_t = t.elapsed();
        let t = Instant::now();
        for _ in 0..accounts { if new::model_is_supported_by_list(&free, &[Some("claude-haiku-4.5")]) { h += 1; } if new::subscription_tier_key(Some("KIRO FREE")) == "free" { h += 1; } }
        let new_t = t.elapsed();
        println!("round {round}: 扫描 {accounts} 个账号（模型判断+等级解析） 旧={:.2}ms 新={:.3}ms 加速={:.0}x 单账号 旧={:.1}us 新={:.2}us h={h}",
            old_t.as_secs_f64()*1e3, new_t.as_secs_f64()*1e3, old_t.as_secs_f64()/new_t.as_secs_f64(),
            old_t.as_secs_f64()*1e6/accounts as f64, new_t.as_secs_f64()*1e6/accounts as f64);
    }
}
