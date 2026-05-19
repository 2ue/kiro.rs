-- 模型计价表(主源 LiteLLM,本地内置兜底)
CREATE TABLE IF NOT EXISTS model_prices (
    -- 模型 id 即主键(provider 前缀可选,如 "claude-opus-4-7" / "anthropic/claude-opus-4-7")
    model_id TEXT PRIMARY KEY,
    display_name TEXT,
    provider TEXT NOT NULL DEFAULT 'anthropic',
    -- 单价(美元 / token,LiteLLM 字段直接对应)
    input_cost_per_token NUMERIC(20, 12),
    output_cost_per_token NUMERIC(20, 12),
    cache_read_input_token_cost NUMERIC(20, 12),
    cache_creation_input_token_cost NUMERIC(20, 12),
    -- 上下文窗口
    max_input_tokens INT,
    max_output_tokens INT,
    -- 元数据
    source TEXT NOT NULL DEFAULT 'litellm',             -- litellm / builtin / manual
    raw JSONB,                                          -- 原始 JSON,便于以后扩字段
    synced_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS model_prices_provider_idx ON model_prices (provider);
CREATE INDEX IF NOT EXISTS model_prices_synced_at_idx ON model_prices (synced_at DESC);
