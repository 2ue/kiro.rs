-- 在线可改的运行时配置(KV 表,值用 jsonb 灵活承接)
-- 静态启动项(host/port/databaseUrl/redisUrl/apiKey/adminApiKey)仍由 config.json 承载
CREATE TABLE IF NOT EXISTS app_config (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    description TEXT,
    -- 最后修改人(手动改时记录,启动播种时为 'system')
    updated_by TEXT NOT NULL DEFAULT 'system',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS app_config_updated_at_idx ON app_config (updated_at DESC);

-- 启动时写入默认值(已存在则不动,允许后续在线修改)
INSERT INTO app_config (key, value, description, updated_by) VALUES
    ('load_balancing_mode',                  '"priority"'::jsonb,        '凭据调度模式: priority | balanced',                'system'),
    ('compat_profile',                       '"claude-code"'::jsonb,     '兼容 profile: claude-code | anthropic-strict | debug', 'system'),
    ('extract_thinking',                     'true'::jsonb,              '非流式响应中是否解析 <thinking> 块',                 'system'),
    ('prompt_cache_simulation_mode',         '"high-cache"'::jsonb,      '本地 prompt cache 模拟模式',                       'system'),
    ('prompt_cache_target_read_ratio',       '0.98'::jsonb,              '高缓存模拟读取比例中心值',                          'system'),
    ('prompt_cache_token_scale',             '1.6'::jsonb,               'high-cache total input 放大倍数',                   'system'),
    ('prompt_cache_max_simulated_input_tokens', '300000'::jsonb,         'high-cache 模拟 total input 上限',                 'system'),
    ('prompt_cache_cap_jitter_min_tokens',   '12000'::jsonb,             '触顶 soft-cap 最小扣减',                            'system'),
    ('prompt_cache_cap_jitter_max_tokens',   '24000'::jsonb,             '触顶 soft-cap 最大扣减',                            'system'),
    ('prompt_cache_scale_min_input_tokens',  '20000'::jsonb,             '基础输入门槛',                                       'system'),
    ('high_cache_threshold',                 '10000'::jsonb,             '管理面板高缓存请求阈值',                             'system'),
    ('default_endpoint',                     '"ide"'::jsonb,             '默认 KiroEndpoint',                                  'system'),
    ('expose_proxy_warnings',                'false'::jsonb,             '是否暴露 x-kiro-rs-warnings 响应头',                'system'),
    ('quota_soft_fail_limit',                '3'::jsonb,                 '402 软超限累计阈值,达到才永久禁用',                  'system'),
    ('quota_cooldown_minutes',               '30'::jsonb,                '402 冷却分钟数',                                     'system'),
    ('pricing_auto_sync_enabled',            'true'::jsonb,              '启动时若 model_prices 为空则自动同步一次',           'system'),
    ('pricing_source_url',                   '"https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"'::jsonb, 'LiteLLM 价格 JSON URL', 'system'),
    ('pricing_bootstrap_done',               'false'::jsonb,             '冷启动同步标记',                                     'system'),
    ('balance_cache_ttl_seconds',            '300'::jsonb,               '上游余额缓存 TTL',                                   'system'),
    ('session_binding_ttl_minutes',          '30'::jsonb,                'sticky session 绑定 TTL',                            'system')
ON CONFLICT (key) DO NOTHING;
