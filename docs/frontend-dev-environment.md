# 前端开发预览环境

本文档只解决一个问题：本地开发时到底看哪个地址。

结论很简单：开发看 Vite 热更新地址，后端只当 API 服务用。后端启动时把三套 UI 都设为 `disabled`，不要用 Rust embedded 页面判断前端源码是否生效。

## 固定地址

| 目标 | 地址 | 启动命令 | 说明 |
|------|------|----------|------|
| 后端 API | `http://127.0.0.1:9022` | 见下方后端启动命令 | 当前仓库本地 `config.json` 的端口。只作为 API 后端使用。 |
| 新版 UI | `http://127.0.0.1:9023/ui/runtime` | `bash scripts/dev-ui.sh ui` | 主要开发入口，改源码后热更新。 |
| Console UI | `http://127.0.0.1:9024/console/config` | `bash scripts/dev-ui.sh console` | Daisy 版本对照，改源码后热更新。 |
| 旧版 Admin UI | `http://127.0.0.1:9025/admin/` | `bash scripts/dev-ui.sh admin` | 旧版对照。打开后点顶部“配置”。 |

如果后端没有跑在 9022，三套前端都支持用 `VITE_API_PROXY_TARGET` 改代理目标：

```bash
VITE_API_PROXY_TARGET=http://127.0.0.1:8990 bash scripts/dev-ui.sh ui
VITE_API_PROXY_TARGET=http://127.0.0.1:8990 bash scripts/dev-ui.sh console
VITE_API_PROXY_TARGET=http://127.0.0.1:8990 bash scripts/dev-ui.sh admin
```

## 推荐启动顺序

先启动本地依赖：

```bash
docker compose -f docker-compose.local-infra.yml up -d
```

再启动后端 API：

```bash
KIRO_NEW_UI_MODE=disabled \
KIRO_CONSOLE_UI_MODE=disabled \
KIRO_ADMIN_UI_MODE=disabled \
  ./target/release/kiro-rs -c config.json --credentials credentials.json
```

这样 `9022` 只承担 API 后端职责。访问 `http://127.0.0.1:9022/ui`、`/console`、`/admin` 会返回 UI disabled，这是预期行为。

最后启动需要看的前端。通常只需要新版 UI：

```bash
bash scripts/dev-ui.sh ui
```

然后打开：

```text
http://127.0.0.1:9023/ui/runtime
```

登录 Key 使用 `config.json` 里的实际 `adminApiKey`，不要按旧文档或记忆里的默认值猜。

## pnpm dev、build、preview 的区别

`pnpm dev` 是开发入口，有热更新。改 TS/TSX/CSS 后浏览器会立刻更新，这是日常开发应该看的环境。

`pnpm build` 是生成 `dist`。这些产物用于发布包、Docker 构建，或者专门验证生产构建。它不是热更新。

`pnpm preview` 是本地查看刚 build 出来的 `dist`，适合检查生产构建结果。它也不是热更新。

## 后端 UI 入口

本地开发不要看后端 UI 入口。按上面的启动方式，后端 UI 已禁用：

```text
http://127.0.0.1:9022/ui       -> ui UI is disabled
http://127.0.0.1:9022/console  -> console UI is disabled
http://127.0.0.1:9022/admin    -> admin UI is disabled
```

这些入口不是开发预览地址。开发只看 `9023`、`9024`、`9025`。

## 发布验证

验证发布产物时，先构建三套前端：

```bash
pnpm --dir ui build
pnpm --dir admin-ui-daisy build
pnpm --dir admin-ui build
```

再重新构建 Rust：

```bash
cargo build --release
```

这时再用发布启动方式访问后端挂载入口才有意义：

```text
http://127.0.0.1:9022/ui
http://127.0.0.1:9022/console
http://127.0.0.1:9022/admin
```

如果只是看源码改动，不要走这条路。

## 常见误区

不要把临时测试端口当成当前 UI 开发入口。本项目本地开发只认这四个端口：后端 API `9022`，新版 UI `9023`，Console UI `9024`，旧版 Admin UI `9025`。

不要在开发时说“我已经 pnpm build 了，为什么页面没变”。开发要看 Vite 热更新地址，`pnpm build` 不是开发预览入口。

不要把 `/cc`、`/cc/v1` 和前端页面混在一起。`/cc` 是 Claude Code API 兼容入口，不是前端预览入口。

不要同时开多个来源不明的 UI 服务来对比同一个问题。先按本文档固定入口复现，再判断问题属于新版 UI、Console UI、旧 Admin UI 还是后端 API。
