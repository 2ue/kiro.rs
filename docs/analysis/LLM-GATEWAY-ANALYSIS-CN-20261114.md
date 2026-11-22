# LLM API 网关架构对比分析 - 中文总结

**分析日期：** 2026-11-14  
**数据源：** 直接克隆代码库分析（非网络摘要）

---

## 🎯 分析对象与规模

| 项目 | 代码量 | 文件数 | 语言 | 特点 |
|------|--------|--------|------|------|
| **LiteLLM** | 3.9M 行 | 9,550 | Python | 企业级完整，159 供应商支持 |
| **Aura** | 53.4K 行 | 116 | Rust | 精简高效，微模块化 ⭐ |
| **Tonic** | ~150K 行 | 497 | Rust | gRPC 框架，模块清晰 |
| **Kiro.rs** | 1.43M 行 | 2,185 | Rust | 中等规模，需优化 |

---

## 💡 核心发现

### 1️⃣ 错误处理模式（优先级：HIGH）

**LiteLLM 方式** - 双维度分类
- ✅ 灵活的分类体系（来源 + 维度）
- ❌ Python 动态类型成本高
- 实现：500+ 行 exceptions.py

**Aura 方式** - 简洁枚举 ⭐ 推荐
```rust
#[derive(Debug, Error)]
pub enum DbError {
    #[error("Connection: {0}")]
    Connection(#[from] sqlx::Error),
    #[error("{entity} not found: {id}")]
    NotFound { entity: &'static str, id: String },
}
```
- ✅ 编译期类型安全
- ✅ thiserror 自动推导
- ✅ 代码简洁（仅 50 行）
- ✅ 错误链自动传播

**Kiro.rs 现状** - 分散处理
- ❌ 4+ 独立 enum（HttpSendError、AdminServiceError 等）
- ❌ 无统一抽象层
- ❌ 未使用 thiserror

**改进方案：** 统一为 `KiroError` enum + thiserror 宏

---

### 2️⃣ 凭据管理（优先级：HIGH）

**LiteLLM** - 企业级复杂度
- 100+ 凭据相关文件
- OAuth2 6+ flow 支持
- 自动密钥轮换
- 15+ 秘密插件
- ❌ 学习成本陡峭

**Aura** - 简洁安全 ⭐ 推荐参考
```rust
// 生成 API 密钥
pub struct GeneratedApiKey {
    pub key: String,          // 一次性显示
    pub key_id: String,       // 公开标识
    pub key_hash: String,     // SHA-256 存储
}

// 凭据加密（信封模式）
MasterKey → DataEncryptionKey
AES-256-GCM 加密供应商凭据
```

**设计原则：**
- 前缀区分环境（kiro_live_ / kiro_test_）
- 密钥原文仅显示一次
- SHA-256 hash 安全存储（不可逆）
- AES-256-GCM 信封加密
- 常数时间比较防时序攻击

**Kiro.rs 现状** - 分散实现，需改进

**改进方案：** 参考 Aura 的 541 行 crypto.rs 实现

---

### 3️⃣ 多供应商处理（优先级：MEDIUM）

**LiteLLM** - 供应商库模式
- 159 个供应商原生支持
- 多种路由策略（shuffle/latency/cost/usage）
- 故障转移链管理
- 部署健康检查

**Aura** - 数据库驱动模式 ⭐ 可观测性强
```sql
routing_decisions 表：
- aura_request_id (追踪)
- selected_provider (供应商选择)
- tier (策略类型)
- shadow (A/B 测试)
- status/latency_ms/cost_usd
```

可查询接口：
```
GET /admin/routing/decisions?tier=simple&limit=10
GET /admin/routing/decisions/{response_id}
```

**Kiro.rs 现状** - Multi-pool 架构已就位

**改进方案：** 添加 routing_decisions 表 + 决策日志

---

### 4️⃣ 测试策略（优先级：MEDIUM）

| 方案 | 文件数 | 覆盖度 | 特点 |
|------|--------|--------|------|
| **LiteLLM** | 2,803 单元 + 1,162 集成 | 高 | pytest 多层配置 |
| **Aura** | 嵌入式测试 | 中 | #[cfg(test)] 轻量 |
| **Kiro.rs** | 分散 | 低 | 需建立框架 |

**改进方案：**
- 建立 tests/ 集成测试目录
- 目标覆盖度 65-70%
- 使用 tokio + sqlx + mockito

---

## 📋 改进路线（4 周计划）

### Week 1：统一错误处理
- 创建 `src/error.rs`
- 定义统一 `KiroError` enum
- 集成 thiserror 宏
- 添加工厂方法简化构造

### Week 2：凭据管理改进
- 创建 `src/crypto/` 模块
- 实现 API 密钥生成 (GeneratedApiKey)
- 实现信封加密 (MasterKey + DEK)
- 防时序攻击 (constant_time_compare)

### Week 3：可观测性
- 添加 `routing_decisions` 数据库表
- 实现路由决策日志记录
- 添加查询接口 (/admin/routing/decisions)
- 支持 A/B 测试追踪

### Week 4：测试框架
- 建立 `tests/` 目录结构
- 集成测试示例 (routing/provider/storage)
- 达成 65-70% 覆盖度

---

## 🔗 参考文件清单

### Aura - 直接参考（推荐）
| 文件 | 行数 | 用途 |
|------|------|------|
| `/crates/aura-db/src/error.rs` | 50 | 错误处理模式 |
| `/crates/aura-core/src/crypto.rs` | 541 | 凭据管理模式 |
| `/crates/aura-proxy/src/routes/routing_decisions.rs` | 300+ | 可观测性架构 |

### LiteLLM - 深入学习
| 文件 | 行数 | 用途 |
|------|------|------|
| `/litellm/exceptions.py` | 200+ | 异常分类体系 |
| `/litellm/router.py` | 23K | 多路由策略 |
| `/litellm/llms/` | 159 目录 | 供应商适配 |

---

## 📊 技术决策矩阵

| 维度 | LiteLLM | Aura | Kiro.rs 建议 |
|------|---------|------|-------------|
| **错误处理** | 分类体系 | thiserror ⭐ | 采用 Aura |
| **凭据管理** | 企业级 | 简洁 ⭐ | 采用 Aura |
| **可观测性** | 策略分散 | 数据库驱动 ⭐ | 采用 Aura |
| **测试** | pytest 深度 | 嵌入式 | 混合模式 |
| **微模块化** | 低 | 高 ⭐ | 长期目标 |

---

## 🚀 立即行动清单

- [ ] **周一** - 创建 `src/error.rs`，统一错误处理
- [ ] **周二** - 创建 `src/crypto/`，参考 Aura 实现
- [ ] **周三** - 添加 `routing_decisions` 表和查询接口
- [ ] **周四** - 建立 `tests/` 集成测试框架

---

**核心结论：**

Aura 的设计哲学（精简 + 安全）值得学习。通过采用其错误处理和凭据管理模式，Kiro.rs 可以在 4 周内显著提升代码质量和可维护性。

建议优先实现 **Week 1-2** 的高优先级改进，为后续可观测性和测试框架建设奠定基础。

---

**分析来源：** 直接克隆 GitHub 代码库  
**生成时间：** 2026-11-14
