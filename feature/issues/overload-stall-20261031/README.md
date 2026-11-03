# 59137 过载停摆（2026-10-31 19:23）

状态：`analysis / reproducing / branch fix/overload-stall-20261031`。

现网 59137 在 0.0.226 上，排队满了以后整个进程变慢：并发槽一直占满，几乎不再调度，RPM 接近 0，最后要重建容器才恢复。10-29 20:02 在 0.0.224 上出过同样的事。

目标：

- 并发够用时不排队，必须能调度到。
- 并发不够时正常排队，排队要持续推进。
- 排队也满了，就快速拒绝，在途请求照常完成，服务不能挂。

无论负载是突然飙升还是缓慢上升，都要满足上面三条，而且不能出现"并发很高、RPM 很低"。

| 文件 | 内容 |
| --- | --- |
| [01-incident.md](01-incident.md) | 现象、时间线、证据 |
| [02-root-cause.md](02-root-cause.md) | 根因分析（代码路径） |
| [03-sub2api-reference.md](03-sub2api-reference.md) | sub2api 的过载处理，哪些可以借鉴 |
| [04-reproduction.md](04-reproduction.md) | 本地复现方案与结果 |
| [05-fix-plan.md](05-fix-plan.md) | 修复计划 |
| [06-test-plan.md](06-test-plan.md) | 测试方案与验收标准 |
