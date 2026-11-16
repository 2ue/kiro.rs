# 前端开发环境

开发访问 Vite 的 `http://127.0.0.1:9023/ui`；后端提供 API。debug 后端的 `/ui` 默认重定向到 Vite，release 后端默认提供嵌入页面。

## 工具与配置

Node.js `22.23.0`、pnpm `11.11.0`、Rust `1.92.0`。从仓库根执行：

```bash
pnpm --dir ui install --frozen-lockfile
docker compose -f docker-compose.local-infra.yml up -d
cp config.example.json config.json
```

编辑 `config.json` 的管理密钥与下游密钥。示例数据库和 Redis 使用本地编排端口 `25432`、`26379`；不需要凭据文件，账号在控制台或管理 API 中创建。

## 启动

一个终端启动后端，将端口明确设为 Vite 默认代理目标 `9022`：

```bash
CLAUDE_POOL_HOST=127.0.0.1 CLAUDE_POOL_PORT=9022 \
  cargo run --bin claude-pool -- -c config.json
```

另一个终端启动前端：

```bash
pnpm --dir ui dev
```

| 目标 | 地址 | 作用 |
|---|---|---|
| API | `http://127.0.0.1:9022` | 后端服务 |
| 控制台 | `http://127.0.0.1:9023/ui/runtime` | 开发入口与热更新 |
| 后端 UI | `http://127.0.0.1:9022/ui` | debug 下重定向到 Vite |

登录使用实际配置中的 `adminApiKey`。数据库已有运行配置时，以数据库中的密钥为准。

如果后端运行在示例端口 `8990`，覆盖 Vite 的 API 代理：

```bash
VITE_API_PROXY_TARGET=http://127.0.0.1:8990 pnpm --dir ui dev
```

后端 API 路由 `/cc/v1` 与前端 `/ui` 分别使用；Claude Code 入口 `/cc` 不是控制台页面。

## 使用 release 后端配合 Vite

```bash
CLAUDE_POOL_UI_MODE=redirect \
CLAUDE_POOL_UI_DEV_SERVER=http://127.0.0.1:9023/ui \
CLAUDE_POOL_PORT=9022 \
  cargo run --release --locked --bin claude-pool -- -c config.json
```

`CLAUDE_POOL_NEW_UI_MODE`、`CLAUDE_POOL_NEW_UI_DEV_SERVER` 是优先读取的同义变量。支持 `embedded`、`redirect`、`proxy`、`filesystem`、`disabled`；文件目录通过 `CLAUDE_POOL_UI_DIR` 指定。

## 验证发布产物

```bash
pnpm --dir ui check
pnpm --dir ui build
cargo build --release --locked --bin claude-pool
cargo run --release --locked --bin claude-pool -- -c config.json
```

访问后端 `/ui` 验证嵌入页面。设置过 redirect 环境变量时先在启动环境中移除相应覆盖。Rust 编译会嵌入当时的 `ui/dist`，前端变更后需要重新构建 release 后端。

`pnpm dev` 提供热更新，`pnpm build` 生成发布文件，`pnpm preview` 预览已构建的文件。一般开发直接使用 Vite，无需反复构建 Rust 发布产物。
