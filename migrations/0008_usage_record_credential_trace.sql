-- 为 usage_records 增加 provider 内部账号尝试链路。
--
-- credential_id 仍表示“最终/主要账号”：成功请求为最终成功账号，失败请求为最后一次
-- 实际尝试账号。数组字段用于还原一次客户端请求内部的 fallback / 429 链路。
ALTER TABLE usage_records
    ADD COLUMN IF NOT EXISTS attempted_credential_ids BIGINT[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS rate_limited_credential_ids BIGINT[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS last_attempted_credential_id BIGINT REFERENCES credentials(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS scheduler_blocked BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS usage_records_last_attempted_credential_idx
    ON usage_records (last_attempted_credential_id, created_at DESC)
    WHERE last_attempted_credential_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS usage_records_attempted_credentials_gin_idx
    ON usage_records USING GIN (attempted_credential_ids);

CREATE INDEX IF NOT EXISTS usage_records_rate_limited_credentials_gin_idx
    ON usage_records USING GIN (rate_limited_credential_ids);
