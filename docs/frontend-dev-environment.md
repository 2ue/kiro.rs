# 前端开发预览环境

本文档只解决一个问题：本地开发时到底看哪个地址。

结论很简单：开发看 Vite 热更新地址，后端只当 API 服务用。debug 构建下，后端 `/ui` 默认会重定向到 Vite 服务，不再依赖 Rust embedded 页面。

## 固定地址

| 目标 | 地址 | 启动命令 | 说明 |
|------|------|----------|------|
| 后端 API | `http://127.0.0.1:9022` | 见下方后端启动命令 | 当前仓库本地 `config.json` 的端口。debug 下 `/ui` 等入口会跳到 Vite。 |
| UI | `http://127.0.0.1:9023/ui/runtime` | `bash scripts/dev-ui.sh ui` | 主要开发入口，改源码后热更新。 |

如果后端没有跑在 9022，前端支持用 `VITE_API_PROXY_TARGET` 改代理目标：

```bash
VITE_API_PROXY_TARGET=http://127.0.0.1:8990 bash scripts/dev-ui.sh ui
```

## 推荐启动顺序

先启动本地依赖：

```bash
docker compose -f docker-compose.local-infra.yml up -d
```

再启动后端 API。debug 构建不需要构建前端 dist：

```bash
cargo run -- -c config.json --credentials credentials.json
```

这样 `9022` 主要承担 API 后端职责。访问 `http://127.0.0.1:9022/ui` 会自动重定向到对应 Vite 服务。

如果你想直接跑 release 二进制但仍走 Vite，可显式指定：

```bash
KIRO_NEW_UI_MODE=redirect \
KIRO_NEW_UI_DEV_SERVER=http://127.0.0.1:9023/ui \
  ./target/release/kiro-rs -c config.json --credentials credentials.json
```

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

debug 构建下，后端 UI 入口默认只是跳转到 Vite：

```text
http://127.0.0.1:9022/ui       -> http://127.0.0.1:9023/ui/
```

热更新由 Vite 提供，所以浏览器最终停留在 `9023` 才是正常现象。release 二进制默认不会这样跳转，仍使用 embedded 页面。

## 发布验证

验证发布产物时，先构建前端：

```bash
pnpm --dir ui build
```

再重新构建 Rust：

```bash
cargo build --release
```

这时再用发布启动方式访问后端挂载入口才有意义：

```text
http://127.0.0.1:9022/ui
```

如果只是看源码改动，不要走这条路。

## 常见误区

不要把临时测试端口当成当前 UI 开发入口。本项目本地开发只认这些端口：后端 API `9022`，UI `9023`。

不要在开发时说“我已经 pnpm build 了，为什么页面没变”。开发要看 Vite 热更新地址，`pnpm build` 不是开发预览入口。

不要把 `/cc`、`/cc/v1` 和前端页面混在一起。`/cc` 是 Claude Code API 兼容入口，不是前端预览入口。

不要同时开多个来源不明的 UI 服务来对比同一个问题。先按本文档固定入口复现，再判断问题属于 UI 还是后端 API。
