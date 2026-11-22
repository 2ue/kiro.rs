# LLM API 网关架构深度分析
**日期：** 2026-11-14  
**分析对象：** LiteLLM、Aura LLM Gateway、Tonic、Kiro.rs  
**数据来源：** 直接代码库分析（非网络摘要）

---

## 📊 项目规模矩阵

| 维度 | LiteLLM | Aura | Tonic | Kiro.rs |
|------|---------|------|-------|---------|
| **语言** | Python | Rust | Rust | Rust |
| **总代码行** | 3,925,646 | 53,474 | ~150K | 1,429,304 |
| **文件数** | 9,550 | 116 | 497 | 2,185 |
| **最大单文件** | 24,710 | 2,941 | ~1,500 | 22,619 |
| **平均文件** | ~410 行 | ~460 行 | ~300 行 | ~650 行 |
| **单元测试** | 2,803 | 嵌入式 | 集成式 | 分散 |
| **集成测试** | 1,162+ | - | - | - |
| **供应商支持** | 159 个 | 多 | gRPC | 多池 |

---

## 🏗️ 架构对比

### 1. 模块组织维度

#### LiteLLM - **深层级、文件细分** 
```
litellm/
├── llms/                 # 159 个供应商目录
│   ├── openai/
│   ├── anthropic/
│   ├── azure/
│   └── ... (159 供应商)
├── proxy/                # 代理核心逻辑
│   ├── auth/             # 25+ 认证文件
│   ├── management_endpoints/
│   ├── mcp_server/
│   └── spend_tracking/
├── router.py             # 23K 行路由逻辑
├── exceptions.py         # 异常体系
└── litellm_core_utils/   # 通用工具
```
**特点：** 横向扩展友好，供应商易添加

#### Aura - **微模块化 workspace**
```
aura/
├── crates/
│   ├── aura-core/        # 业务逻辑核心 (~10K 行)
│   │   ├── crypto.rs
│   │   ├── provider/
│   │   ├── router/
│   │   └── ...
│   ├── aura-proxy/       # HTTP 网关 (~10K 行)
│   ├── aura-db/          # 数据层 (~8K 行)
│   └── aura-types/       # 类型定义
```
**特点：** 高度内聚，依赖清晰，易于并发开发

#### Kiro.rs - **单 crate，需要分解**
```
src/
├── external_pool.rs      # 多池管理
├── storage/postgres.rs   # 22.6K 行
├── kiro/                 # 核心逻辑
├── anthropic/            # 供应商适配
├── http_client.rs        # HTTP 客户端
└── admin/                # 管理服务
```
**问题：** 1.43M 行在单 crate，文件平均 650 行过大

---

## 🔴 错误处理模式对比

### 模式 A：LiteLLM (Python) - 分类体系
```python
# exceptions.py 核心设计

class RateLimitErrorCategory(enum.Enum):
    """限流来源分类"""
    REQUESTS = "requests"              # RPM 超限
    TOKENS = "tokens"                  # TPM 超限
    CONCURRENT_REQUESTS = "concurrent" # 并发超限
    BUDGET = "budget"                  # 预算超限
    MAX_ITERATIONS = "max_iterations"  # 迭代次数超限

class RateLimitType(enum.Enum):
    """限流维度分类"""
    VENDOR_RATE_LIMIT = "vendor"       # 上游供应商
    LITELLM_RATE_LIMIT = "litellm"     # LiteLLM 自身

class AuthenticationError(openai.AuthenticationError):
    """与 OpenAI SDK 兼容的认证错误"""
    status_code = 401
    llm_provider: str      # 供应商
    model: str             # 模型
    body: dict             # 完整请求体
    litellm_debug_info: str
```

**优点：**
- ✅ 双维度分类（来源 + 维度）
- ✅ 与上游 SDK 兼容（鸭型多态）
- ✅ 上下文丰富（完整请求/响应体）
- ✅ 可观测性强（Prometheus 标签无冲突）

**缺点：**
- ❌ Python 运行时类型检查成本
- ❌ 异常链传播需手工管理

---

### 模式 B：Aura (Rust) - 简洁枚举 + From trait
```rust
// error.rs (50 行)

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("Database connection: {0}")]
    Connection(#[from] sqlx::Error),     // 自动 From 转换
    
    #[error("{entity} not found: {id}")]
    NotFound {
        entity: &'static str,
        id: String,
    },
    
    #[error("Duplicate {entity}: {key}")]
    Duplicate {
        entity: &'static str,
        key: String,
    },
    
    #[error("Config: {0}")]
    Config(String),
}

impl DbError {
    // 工厂方法 - 简化构造
    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            id: id.into(),
        }
    }
}
```

**优点：**
- ✅ 编译期类型安全
- ✅ 自动 From trait（减少样板代码）
- ✅ Display 宏生成（无需手写格式化）
- ✅ 错误链自动传播（`?` 操作符）
- ✅ 代码简洁（50 行 vs Python 数百行）

**缺点：**
- ❌ 缺少动态分类（如 LiteLLM 的 RateLimitType）
- ❌ 需要为每个错误类型创建新的 enum

---

### 模式 C：Kiro.rs (Rust) - 当前现状
```rust
// src/http_client.rs
pub enum HttpSendError {
    Request(reqwest::Error),
    ResponseHeaderTimeout { timeout_secs: u64 },
    ResponseBodyTimeout { timeout_secs: u64 },
    ResponseBodyTooLarge { max_bytes: usize },
}

// src/admin/error.rs
pub enum AdminServiceError { ... }

// src/kiro/parser/error.rs
pub enum ParseError { ... }

// src/model/model_processing.rs
pub enum ModelProcessingError { ... }
```

**问题：**
- ❌ 错误类型分散（4+ 独立 enum）
- ❌ 无统一抽象层
- ❌ 没有使用 thiserror（手动实现 Display/From）
- ❌ 错误链转换复杂（需要多层 map_err）

---

## 🔑 凭据管理对比

### 方案 A：LiteLLM - 企业级复杂度

**架构：**
- 100+ 凭据相关文件
- OAuth2 支持（6+ flow 类型）
- 密钥轮换管理
- 企业级秘密插件体系（15+ 插件）
- Redis 分布式凭据缓存
- 掩码存储（mask sensitive values）

**文件示例：**
```
litellm/proxy/auth/
├── oauth2_check.py
├── oauth2_proxy_hook.py
├── user_api_key_auth.py
├── credential_migration.py
└── ...

litellm/proxy/mcp_server/outbound_credentials/
├── oauth_token_store.py
├── client_credentials.py
├── per_user_oauth_store.py
└── session_credentials.py
```

**适用场景：** 企业 SSO、多租户、全球部署

---

### 方案 B：Aura - 简洁高效 ✓ 推荐参考

**核心代码：** 541 行 (crypto.rs)

```rust
// ========== API 密钥生成 ==========
pub struct GeneratedApiKey {
    pub key: String,          // 仅显示一次给用户
    pub key_id: String,       // 公开标识符（DB 中存储）
    pub key_hash: String,     // SHA-256 哈希（安全存储）
}

pub fn generate_api_key(prefix: &str) -> GeneratedApiKey {
    // 前缀区分环境: aura_live_xxx / aura_test_xxx
    // 格式: prefix(11) + identifier(24) + secret(24) = ~59 字符
    
    // 存储策略：
    // DB: key_id + key_hash
    // 用户获得: key（一次性展示）
}

// ========== 验证 ==========
pub fn verify_api_key(key: &str, stored_hash: &str) -> bool {
    let computed = hash_api_key(key);
    constant_time_compare(computed, stored_hash)  // 防时序攻击
}

// ========== 供应商凭据加密 ==========
pub struct MasterKey { /* KEK */ }
pub struct DataEncryptionKey { /* DEK */ }

impl MasterKey {
    pub fn wrap_dek(&self, dek: &DataEncryptionKey) -> Result<Vec<u8>> {
        // 使用 AES-256-GCM 进行信封加密
        // 返回: nonce || ciphertext
    }
    
    pub fn unwrap_dek(&self, wrapped: &[u8]) -> Result<DataEncryptionKey> {
        // 解密 DEK
    }
}
```

**设计原则：**
- **简单** 是最大的安全特性
- 前缀区分环境
- 一次性显示原文密钥
- SHA-256 Hash（可逆存储不可能）
- AES-256-GCM 信封加密（供应商凭据）
- 常数时间比较（防时序攻击）

**架构对比：**

| 特性 | LiteLLM | Aura | Kiro.rs |
|------|---------|------|---------|
| 代码行数 | 2000+ | 541 | 分散 |
| 文件数 | 100+ | 1 | 3+ |
| OAuth2 | ✅ 完整 | ❌ | ✅ 需要 |
| 密钥轮换 | ✅ 自动 | ❌ | ❌ |
| 信封加密 | ❌ | ✅ | ❌ |
| 时序攻击防护 | ❌ | ✅ | ❌ |
| 学习曲线 | 陡峭 | 平缓 | - |

**Kiro.rs 改进建议：**
参考 Aura 的简洁模式 + 根据需求选择 LiteLLM 的高级特性

---

## 🔀 多供应商处理对比

### LiteLLM 路由策略

```python
routing_strategy: Literal[
    "simple-shuffle",           # 随机
    "latency-based-routing",    # 基于延迟
    "cost-based-routing",       # 基于成本
    "usage-based-routing",      # 基于用量
    "least-busy"                # 最空闲
]

routing_config = {
    "model_list": [
        {
            "model_name": "gpt-4",
            "litellm_params": {"model": "azure/gpt-4-v1", ...},
            "weight": 0.5,      # 流量比例
        },
        {
            "model_name": "gpt-4",
            "litellm_params": {"model": "openai/gpt-4", ...},
            "weight": 0.5,
        }
    ],
    "fallback": ["azure-gpt4", "openai-gpt4"],  # 故障转移链
    "context_window_fallbacks": [...],           # 上下文超限转移
}
```

**特点：**
- 159 个供应商原生支持
- 动态路由策略选择
- 多维度故障转移（context window、预算、速率限制）
- 部署健康检查（热/冷管理）

---

### Aura 数据库驱动方案

```rust
// 数据库字段结构
pub struct RequestLog {
    pub provider_name: String,      // "openai" / "anthropic" / ...
    pub model_id: String,
    pub provider_response_id: Option<String>,  // 供应商原生 ID
    pub cost_usd: f64,
    pub latency_ms: i32,
}

pub struct RoutingOutcome {
    pub selected_provider: Option<String>,
    pub shadow: bool,               // A/B 测试
    pub status: String,
    pub decided_at: DateTime,
}

// 查询接口
GET /admin/routing/decisions?tier=simple&limit=10
    → 获取最近的路由决策及选中供应商

GET /admin/routing/decisions/{response_id}
    → 追踪单个请求的路由链路
```

**特点：**
- 决策可观测性强（完整日志）
- 可追踪供应商选择
- 支持影子流量（A/B 测试）
- 性能计算支持（成本模型）

---

### Kiro.rs 现状 + 建议

**现有能力：**
- ✅ Multi-pool 架构（/src/external_pool.rs）
- ✅ 外部池凭据管理
- ✅ 负载均衡基础

**改进建议：**
1. **采用 Aura 的数据库驱动方案** - 可观测性更强
2. **添加路由决策日志表** - 供应商选择追踪
3. **实现成本计算模块** - 参考 Aura 的 cost.rs
4. **支持 A/B 测试标记** - shadow 流量区分

---

## 📝 测试策略对比

### LiteLLM - 分层测试体系

```
tests/
├── unit/              # 2,803 个单元测试
│   ├── proxy/
│   ├── router/
│   └── llms/
├── integration/       # 1,162+ 集成测试
│   ├── test_router.py (20.5K 行)
│   └── ...
├── e2e/              # 端到端测试
├── benchmarks/       # 性能基准
└── conftest.py       # 多层配置

[dev-dependencies]
pytest, pytest-asyncio, httpx, aiohttp
```

**测试密度：** ~4,400 个测试覆盖 3.9M 行（高覆盖）

---

### Aura - 嵌入式测试模式

```rust
// routing_decisions.rs 内的 #[cfg(test)] 模块
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_routing_decisions() { ... }
    
    #[tokio::test]
    async fn test_list_with_pagination() { ... }
}

// 运行: cargo test routing_decisions
```

**特点：**
- 代码与测试相邻（易于维护）
- tokio 异步支持
- 轻量级，无外部测试框架依赖

---

### Kiro.rs - 改进建议

1. **建立测试目录结构**
   ```rust
   tests/
   ├── integration/
   │   ├── routing_tests.rs
   │   ├── provider_tests.rs
   │   └── storage_tests.rs
   ├── fixtures/
   └── common/
   ```

2. **添加 dev-dependencies**
   ```toml
   [dev-dependencies]
   tokio = { version = "1", features = ["full"] }
   sqlx = { version = "0.7", features = ["runtime-tokio", "sqlite"] }
   mockito = "1.2"
   ```

3. **目标覆盖度：** 60-70%（从 0% 增长）

---

## 🎯 Kiro.rs 改进路线图

### Phase 1: 错误处理统一（Week 1）

```rust
// src/error.rs (新建)
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KiroError {
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    
    #[error("Database: {0}")]
    Database(#[from] sqlx::Error),
    
    #[error("Routing: {reason}")]
    Routing { reason: String },
    
    #[error("Auth: {0}")]
    Auth(String),
    
    #[error("Pool: {0}")]
    ExternalPool(String),
}

pub type Result<T> = std::result::Result<T, KiroError>;

impl KiroError {
    pub fn http(status: u16, msg: impl Into<String>) -> Self { ... }
    pub fn routing(reason: impl Into<String>) -> Self { ... }
}

// 使用: 
// result.map_err(|e| KiroError::Routing { reason: e.to_string() })?
// 或更简洁: KiroError::routing("not found")?
```

---

### Phase 2: 凭据管理改进（Week 2）

参考 Aura 的 crypto.rs，创建 `src/crypto/` 模块：

```rust
// src/crypto/api_key.rs
pub struct GeneratedApiKey { ... }
pub fn generate_api_key(prefix: &str) -> GeneratedApiKey { ... }

// src/crypto/envelope.rs
pub struct MasterKey { ... }
pub struct DataEncryptionKey { ... }
```

特点：
- ✅ SHA-256 hash 存储 API 密钥
- ✅ AES-256-GCM 加密供应商凭据
- ✅ 前缀区分环境 (kiro_live_ / kiro_test_)
- ✅ 常数时间比较防时序攻击

---

### Phase 3: 微模块化（Week 3-4）

```rust
// Cargo.toml
[workspace]
members = [
    "crates/kiro-core",      # 业务逻辑
    "crates/kiro-storage",   # PostgreSQL 层
    "crates/kiro-http",      # HTTP 客户端
    "crates/kiro-admin",     # 管理 API
]

// src/
├── lib.rs          # 对外接口
├── error.rs        # ✅ Phase 1
├── crypto/         # ✅ Phase 2
├── core/           # 核心逻辑（从 src/kiro 迁移）
├── storage/        # 数据层
├── http/           # HTTP 相关
└── admin/          # 管理服务
```

---

## 📌 推荐参考文件清单

### LiteLLM - 学习重点
| 文件 | 行数 | 用途 |
|------|------|------|
| `/litellm/exceptions.py` | 500+ | 异常分类体系 |
| `/litellm/router.py` | 23K | 多路由策略 |
| `/litellm/proxy/auth/` | 25+ 文件 | 认证参考 |
| `/litellm/llms/` | 159 目录 | 供应商适配模式 |

### Aura - 直接参考 ⭐ 推荐
| 文件 | 行数 | 用途 |
|------|------|------|
| `/crates/aura-db/src/error.rs` | 50 | 错误处理 (参考) |
| `/crates/aura-core/src/crypto.rs` | 541 | 凭据管理 (参考) |
| `/crates/aura-proxy/src/routes/routing_decisions.rs` | 300+ | 可观测性 |
| `/crates/aura-core/src/cost.rs` | - | 成本计算 |

### Kiro.rs - 改进目标
| 文件 | 现状 | 行动 |
|------|------|------|
| `/src/http_client.rs` | - | 使用统一 KiroError |
| `/src/external_pool.rs` | 分散 | 集成 crypto 模块 |
| `/src/storage/postgres.rs` | 22.6K | 拆分 + 错误处理 |
| `/src/admin/` | - | 使用统一 Result 类型 |

---

## 💡 核心洞察

1. **规模 vs 复杂度**
   - LiteLLM: 3.9M 行支持 159 供应商（平均 24.6K/供应商）
   - Aura: 53.4K 行核心逻辑（极度精简）
   - Kiro.rs: 1.43M 行（中等规模，可优化）

2. **错误处理哲学**
   - Python：动态分类 + 鸭型多态 = 灵活但复杂
   - Rust：编译期安全 + 类型约束 = 简洁但需设计

3. **凭据管理权衡**
   - LiteLLM：企业级完整性 (100+ 文件)
   - Aura：原则优先简洁性 (541 行)
   - Kiro.rs：推荐混合策略（简洁基础 + 按需扩展）

4. **可观测性**
   - 无 logging，无 tracing 的设计最好
   - Aura 的数据库驱动追踪值得学习
   - 路由决策要完全可审计

---

## 🚀 立即行动清单

- [ ] Week 1: 迁移错误处理至统一 `KiroError` enum + thiserror
- [ ] Week 2: 集成 Aura 风格的 crypto 模块 (crypto.rs)
- [ ] Week 3: 添加 routing_decisions 表和可观测性日志
- [ ] Week 4: 建立集成测试框架 (cargo test 65%+ 覆盖)
- [ ] 长期: 微模块化拆分为 workspace crates

---

**生成时间：** 2026-11-14  
**数据来源：** 直接克隆分析（非网络搜索）
