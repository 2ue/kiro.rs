-- 凭据表(Kiro 认证主体)
-- 一行一个 Kiro 账号 / API Key 凭据,运行时通过 priority 排序选择
CREATE TABLE IF NOT EXISTS credentials (
    id BIGSERIAL PRIMARY KEY,
    -- 账号身份
    auth_method TEXT NOT NULL,                          -- social / idc / api_key
    email TEXT,
    machine_id TEXT,                                    -- 64 位字符串,锁定客户端指纹
    profile_arn TEXT,
    subscription_title TEXT,                            -- KIRO PRO / KIRO PRO+ / KIRO FREE 等
    endpoint TEXT NOT NULL DEFAULT 'ide',               -- 该凭据走哪个 KiroEndpoint

    -- OAuth 字段(API Key 凭据时为 NULL)
    refresh_token TEXT,
    access_token TEXT,
    expires_at TIMESTAMPTZ,
    client_id TEXT,
    client_secret TEXT,
    auth_region TEXT,
    api_region TEXT,

    -- API Key 字段(OAuth 凭据时为 NULL)
    kiro_api_key TEXT,

    -- 代理(凭据级,覆盖全局)
    proxy_url TEXT,
    proxy_username TEXT,
    proxy_password TEXT,

    -- 哈希用于前端去重(注意:运行时计算,但落库提速)
    refresh_token_hash TEXT,                            -- SHA-256 hex
    api_key_hash TEXT,

    -- 调度状态
    priority INT NOT NULL DEFAULT 0,                    -- 数字越小优先级越高
    disabled BOOLEAN NOT NULL DEFAULT FALSE,
    disabled_reason TEXT,                               -- 'too_many_failures' / 'quota_exceeded' / 'too_many_refresh_failures' / 'manual'
    failure_count INT NOT NULL DEFAULT 0,
    refresh_failure_count INT NOT NULL DEFAULT 0,
    success_count BIGINT NOT NULL DEFAULT 0,
    last_used_at TIMESTAMPTZ,

    -- 时间戳
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT credentials_auth_method_chk CHECK (auth_method IN ('social', 'idc', 'api_key'))
);

CREATE UNIQUE INDEX IF NOT EXISTS credentials_refresh_hash_uniq
    ON credentials (refresh_token_hash) WHERE refresh_token_hash IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS credentials_api_key_hash_uniq
    ON credentials (api_key_hash) WHERE api_key_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS credentials_priority_idx ON credentials (priority, id);
CREATE INDEX IF NOT EXISTS credentials_disabled_idx ON credentials (disabled, priority);
