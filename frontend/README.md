# kiro.rs Frontend (新版控制台)

基于 React 18 + Vite 6 + TypeScript + Tailwind + Radix UI / shadcn/ui 的全新前端,与 `admin-ui/` 并存。

## 开发

```bash
cd frontend
pnpm install
pnpm dev          # http://127.0.0.1:9023
```

`/api` 与 `/v1` 由 vite dev server 反代到 `VITE_API_PROXY_TARGET`(默认 `http://127.0.0.1:9022`),登录后会带 `x-api-key` 调用 Admin API。

## 构建

```bash
pnpm build        # 产物在 frontend/dist
```

## 目录

```
src/
├── components/
│   ├── ui/         shadcn/ui 风格的 Radix 组件
│   ├── layout/     侧边栏 / 顶栏 / AppShell
│   ├── login/      登录页
│   ├── dashboard/  仪表盘(占位)
│   ├── credentials usage pricing settings   各业务页(占位)
├── routes/         react-router 路由
├── store/          zustand 状态(auth, preferences)
├── lib/            api / utils / storage
└── types/          API 类型定义
```

## 当前阶段

**阶段一·1 已完成**:脚手架 + 路由 + 布局 + 登录 + 占位页面。

后续阶段(凭据 / 用量 / 计价 / 设置)将逐步接入后端 PG/Redis 后的新 API。
