-- 把 quota_strike_count 与 cooldown_until 持久化到 credentials 表,
-- 之前只存于内存的 CredentialEntry,重启会被清零导致"3 次冷却"策略被绕过。

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS quota_strike_count INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cooldown_until TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS credentials_cooldown_idx ON credentials (cooldown_until)
    WHERE cooldown_until IS NOT NULL;
