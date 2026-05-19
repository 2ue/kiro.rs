-- 管理员操作审计:谁/在何时/做了什么/影响了哪条记录
-- 关键路径(set_disabled / set_priority / delete / refresh / config 写入 / pricing sync 等)插入
CREATE TABLE IF NOT EXISTS admin_actions (
    id BIGSERIAL PRIMARY KEY,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    actor TEXT NOT NULL DEFAULT 'admin',
    action TEXT NOT NULL,                            -- set_disabled / set_priority / delete_credential / force_refresh / config_update / pricing_sync / clear_usage / ...
    target_type TEXT,                                -- credential / config / pricing / usage
    target_id TEXT,                                  -- 被操作对象 id(凭据 id / 配置 key / model_id 等)
    payload JSONB,                                   -- 关键参数(改前/改后值等)
    note TEXT
);

CREATE INDEX IF NOT EXISTS admin_actions_time_idx ON admin_actions (occurred_at DESC);
CREATE INDEX IF NOT EXISTS admin_actions_target_idx
    ON admin_actions (target_type, target_id, occurred_at DESC)
    WHERE target_type IS NOT NULL;
CREATE INDEX IF NOT EXISTS admin_actions_action_idx ON admin_actions (action, occurred_at DESC);
