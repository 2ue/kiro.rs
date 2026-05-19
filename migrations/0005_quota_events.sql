-- 配额超限事件表(支撑"3 次冷却"判定 + 历史可视化)
CREATE TABLE IF NOT EXISTS quota_events (
    id BIGSERIAL PRIMARY KEY,
    credential_id BIGINT NOT NULL REFERENCES credentials(id) ON DELETE CASCADE,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    kind TEXT NOT NULL,                                 -- soft_402 / hard_disabled / cooldown_recovered / manual_reset
    reason TEXT,                                        -- 上游 reason 字符串(MONTHLY_REQUEST_COUNT 等)
    upstream_status SMALLINT,                           -- 触发状态码
    cooldown_until TIMESTAMPTZ,                         -- 本次事件分配的冷却结束时间
    note TEXT,
    CONSTRAINT quota_event_kind_chk CHECK (kind IN ('soft_402', 'hard_disabled', 'cooldown_recovered', 'manual_reset'))
);

CREATE INDEX IF NOT EXISTS quota_events_credential_time_idx
    ON quota_events (credential_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS quota_events_kind_time_idx
    ON quota_events (kind, occurred_at DESC);
