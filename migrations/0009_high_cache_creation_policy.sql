-- high-cache 创建量独立策略:
-- 默认多数 creation 按较小比例模拟,少数请求仍允许产生大 creation;read 仍走原高缓存比例。
INSERT INTO app_config (key, value, description, updated_by) VALUES
    ('prompt_cache_creation_ratio_min',          '0.12'::jsonb, 'high-cache creation 普通场景最小比例',       'system'),
    ('prompt_cache_creation_ratio_max',          '0.35'::jsonb, 'high-cache creation 普通场景最大比例',       'system'),
    ('prompt_cache_creation_burst_probability',  '0.10'::jsonb, 'high-cache creation 使用完整高缓存比例的概率', 'system')
ON CONFLICT (key) DO NOTHING;
