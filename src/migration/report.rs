#[derive(Debug, Default)]
pub(super) struct MigrationReport {
    pub changes: Vec<String>,
    pub highlights: Vec<String>,
    pub errors: Vec<String>,
    pub local_injections: Vec<String>,
    pub normalized_accounts: Vec<String>,
    pub enabled_features: Vec<String>,
    pub changed_defaults: Vec<String>,
    pub concurrency: u64,
}

impl MigrationReport {
    pub fn moved(&mut self, from: &str, to: &str) {
        self.changes.push(format!("[映射] {from} → {to}"));
    }
    pub fn discarded(&mut self, path: &str, value: &serde_json::Value) {
        self.changes.push(format!("[丢弃] {path} = {value}"));
    }
    pub fn render(&self, apply: bool) -> String {
        let mut lines = vec![format!(
            "旧库迁移差异报告（{}；全量覆盖，按 name/model 自然键匹配）",
            if apply {
                "准备应用"
            } else {
                "dry-run，只预览"
            }
        )];
        lines.extend(self.highlights.iter().map(|s| format!("【重点确认】{s}")));
        lines.extend(self.changes.clone());
        if self.errors.is_empty() {
            lines.push("[校验] 通过现有配置、路由与数据校验".into());
        } else {
            lines.extend(self.errors.iter().map(|s| format!("[校验失败] {s}")));
            lines.push("[拒绝应用] 请先修正以上问题".into());
        }
        lines.push("行为差异提示（固定检查项）：".into());
        lines.push(format!(
            "1. 所有流量走账号池；账号池并发总量：{}（各账号 max_concurrent_requests 之和）。",
            self.concurrency
        ));
        lines.push(format!(
            "2. 原本只对本地生效的注入项现在对账号生效：{}。",
            join(&self.local_injections)
        ));
        lines.push(format!(
            "3. 请求体不再规范化；原 request_body_mode=normalized 的账号：{}。",
            join(&self.normalized_accounts)
        ));
        lines.push(format!(
            "4. 未显式设置时的默认值变化：{}。",
            join(&self.changed_defaults)
        ));
        lines.push(format!("5. 原全局路由功能现在默认关闭；已搬入 routeFeatures.defaults 的原开启项：{}；请确认哪些路由要开。", join(&self.enabled_features)));
        lines.join("\n") + "\n"
    }
}

fn join(items: &[String]) -> String {
    if items.is_empty() {
        "无".into()
    } else {
        items.join("、")
    }
}
