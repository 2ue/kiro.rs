-- 用量记录表(每次 Anthropic-compatible 请求一条)
CREATE TABLE IF NOT EXISTS usage_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 请求标识
    request_id TEXT,                                    -- x-request-id,客户端可凭此排查
    endpoint TEXT NOT NULL,                             -- ide / cli / ...
    stream BOOLEAN NOT NULL,
    model TEXT NOT NULL,
    model_provider TEXT NOT NULL DEFAULT 'anthropic',
    conversation_id TEXT,
    credential_id BIGINT REFERENCES credentials(id) ON DELETE SET NULL,
    credential_label TEXT,                              -- 冗余存,凭据删除后仍可查
    -- 状态
    status TEXT NOT NULL,                               -- success / error / stream_error / upstream_timeout / client_dropped
    usage_source TEXT NOT NULL,                         -- upstream_metadata / local_prompt_cache / context_estimate / request_estimate / none
    error_type TEXT,
    error_message TEXT,
    -- token
    total_input_tokens INT NOT NULL DEFAULT 0,
    compat_input_tokens INT NOT NULL DEFAULT 0,
    billable_input_tokens INT NOT NULL DEFAULT 0,
    output_tokens INT NOT NULL DEFAULT 0,
    cache_read_input_tokens INT NOT NULL DEFAULT 0,
    cache_creation_input_tokens INT NOT NULL DEFAULT 0,
    cache_creation_5m_input_tokens INT NOT NULL DEFAULT 0,
    cache_creation_1h_input_tokens INT NOT NULL DEFAULT 0,
    -- 成本
    cost_usd NUMERIC(14, 8),                            -- 当次请求的美元成本(可空,价格未知时不写)
    -- 客户端信息
    client_user_agent TEXT,
    client_ip INET,
    -- 性能
    duration_ms BIGINT NOT NULL DEFAULT 0,
    -- 标记
    simulated BOOLEAN NOT NULL DEFAULT FALSE,
    sticky_bound BOOLEAN NOT NULL DEFAULT FALSE,
    fallback_from_sticky BOOLEAN NOT NULL DEFAULT FALSE,

    CONSTRAINT usage_status_chk CHECK (status IN ('success', 'error', 'stream_error', 'upstream_timeout', 'client_dropped')),
    CONSTRAINT usage_source_chk CHECK (usage_source IN ('upstream_metadata', 'local_prompt_cache', 'context_estimate', 'request_estimate', 'none'))
);

-- 时间倒序索引(列表/分页常态)
CREATE INDEX IF NOT EXISTS usage_records_created_at_desc_idx ON usage_records (created_at DESC);
-- 按账号 + 时间(单账号详情)
CREATE INDEX IF NOT EXISTS usage_records_credential_created_idx ON usage_records (credential_id, created_at DESC) WHERE credential_id IS NOT NULL;
-- 按模型聚合
CREATE INDEX IF NOT EXISTS usage_records_model_idx ON usage_records (model);
-- 按会话
CREATE INDEX IF NOT EXISTS usage_records_conversation_idx ON usage_records (conversation_id) WHERE conversation_id IS NOT NULL;
-- request_id 可选唯一(部分 NULL,用条件唯一)
CREATE INDEX IF NOT EXISTS usage_records_request_id_idx ON usage_records (request_id) WHERE request_id IS NOT NULL;
-- 全文/筛选用(状态 + 时间)
CREATE INDEX IF NOT EXISTS usage_records_status_created_idx ON usage_records (status, created_at DESC);
