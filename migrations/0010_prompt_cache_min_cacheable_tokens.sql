-- 本地 prompt-cache 模拟的最小可缓存目标 token 门槛。
-- high-cache 与 local-prompt-cache 共用,不再按 opus/非 opus 区分。
INSERT INTO app_config (key, value, description, updated_by) VALUES
    ('prompt_cache_min_cacheable_tokens', '1024'::jsonb, '本地 prompt-cache 模拟最小可缓存 token 门槛', 'system')
ON CONFLICT (key) DO NOTHING;
