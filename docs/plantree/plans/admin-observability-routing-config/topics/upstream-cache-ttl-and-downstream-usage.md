# 上游缓存 TTL 与下游 usage 展示分离

状态：实施并专项验证中。当前会话授权 main 改造、forced-main 直接合并 main、claude-main 独立移植，并分别发布。

## 行为契约

- 路由“下游 usage 写入时长”决定下游缓存写入的 5m/1h 桶与该份 usage 的展示计价，不选择账号，不修改优先级、并发、队列和重试。
- 外部账号的 Anthropic 请求里，协议位置上已有的 `cache_control.type=ephemeral` 始终显式使用 `ttl="5m"`：已有 1h 或其他 TTL 覆盖成 5m，原本缺 TTL 时主动补齐 5m。
- 不新增缓存断点；不修改工具 input、JSON schema、metadata 或普通文本里看起来像缓存标记的对象。
- 归一化同时覆盖 raw_passthrough 和 normalized 请求准备路径，以及流式、非流式和 usage 透传/整形账号。
- 客户端原始请求保留，因此下游“跟随请求”模式仍读取客户端原始 1h/5m 标记；出站副本的固定 5m 不覆盖展示依据。
- 本地 Kiro 使用 conversationState/cachePoint 协议，不发送 Anthropic 的 cache_control 或 ttl；本地展示选项不改 Kiro 出站协议。
- 原始 usage 和原始成本仍根据上游实际响应，不伪造为 1h。
- 本地与外部模拟缓存状态写入寿命固定5分钟，并继续受独立 entryTtl 上限限制；展示选1h不得延长后续缓存命中状态。旧实现将展示TTL带入成功提交bounds，已解耦。

## 实现

`external_pool/body_pipeline.rs::prepare_request` 在所有请求体准备分支汇合后，调用 `anthropic/request_facts.rs::rewrite_upstream_cache_ttl_5m`。后者沿协议内容路径有界扫描，只替换 TTL 值或在已有 ephemeral 标记内插入 TTL；其余字节保留。无修改时复用原 Bytes，不做整包 JSON Value 往返。客户端 effective_raw_body 与探测信息不变。

UI 将“缓存写入时长”更名为“下游 usage 写入时长”，注明外部上游固定显式 5m，区分本地 Kiro 展示和外部池展示。

## 验证

- TTL 联合回归：五种展示模式 × 客户端缺TTL/5m/1h × 流式/非流式 × usage 整形/透传 × raw/normalized 请求体，共120次出站检查；另检查下游拆分与原始成本快照。
- 扫描器回归覆盖顶层、system、tools、消息、tool_result嵌套内容、escaped key/value、缺字段、既有5m、不新增标记及深度边界；schema/input/metadata/文本保留。
- 原始请求体/模型映射既有回归、UI时长标签/策略导入导出及TypeScript检查。
- 版本与发布结果由发版记录补充；本功能提交不修改依赖或版本。
