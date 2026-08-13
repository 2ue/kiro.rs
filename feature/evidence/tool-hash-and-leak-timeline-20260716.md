# Tool Hash And Leak Timeline Evidence

Date: 2026-07-16

Role: 复核工具名 hash 指纹与 transcript 泄漏频率变化是否属于同一引入点

Status: `verified-git-history`

## 结论

当前可见的 `bashHashxxxxxxxx`/`readHashxxxxxxxx`/`editHashxxxxxxxx` 是项目工具名映射的确定性指纹，但不是 transcript 泄漏的起点，也不是 v0.0.95 之后才出现。

Git 历史显示两个独立阶段：

| 阶段 | Commit | 日期 | 格式与行为 |
| --- | --- | --- | --- |
| 初始短名映射 | `551b91f1539668a36c2d9e1371d8cf738b0ae7b9` | 2026-03-31 | 超长工具名改为 `prefix_<8 hex>`，并保存短名到原名映射 |
| 当前 `Hash` 指纹 | `df60befe873d150e729519e68f9a2314fb24938e` | 2026-05-29 | 改为 `prefixHash<8 hex>`；包含分隔符等非法名称也会映射；同一 commit 还加入 malformed tool result textify |

`git merge-base --is-ancestor` 对 `551b91f -> v0.0.94/v0.0.95` 均返回成功；直接读取两个 tag 的 `src/anthropic/converter/tools.rs` 也都能看到 `TOOL_HASH_MARKER = "Hash"` 和 `prefix + Hash + 8 hex` 实现。`df60bef` 同样被 `v0.0.94`、`v0.0.95` 包含。

因此：

- “95 之后才引入 hash 映射”不成立；
- “过去泄漏但指纹不同”与 `_xxxxxxxx`、普通工具名、placeholder/textify 等历史形态一致；
- 频率上升需要从 2026-07-15 的 placeholder/prompt/history/retry 变化及已有会话/持久配置叠加解释，不能由 hash 格式单独解释。

## 复核命令

```bash
git show --no-patch --format='%H%n%ad%n%s' --date=iso-strict 551b91f
git show 551b91f:src/anthropic/converter.rs
git show --no-patch --format='%H%n%ad%n%s' --date=iso-strict df60bef
git show df60bef -- src/anthropic/converter.rs
git merge-base --is-ancestor 551b91f v0.0.94
git merge-base --is-ancestor 551b91f v0.0.95
git show v0.0.94:src/anthropic/converter/tools.rs
git show v0.0.95:src/anthropic/converter/tools.rs
git log --all -S'Tool results provided.' -- src
git log --all -S'readHash' -- src config.example.json
```

## 与根因的关系

工具映射本身是 Kiro 63 字符/字符集兼容机制。只要请求/响应反向映射完整，mapped name 不应进入最终可见正文。真正把它变成用户可见文本的已确认路径包括：

1. malformed/orphan/mismatch/duplicate tool result 被复制成普通正文；
2. history trim 拆开 tool pair 后由 repair 再次 textify；
3. `Tool results provided.`/`Continue` 这类命令型占位进入模型上下文；
4. text-only sanitizer 漏掉 thinking/external SSE；
5. 错误期多层 retry 放大了模型再次接触并复述污染历史的机会。

这也是修复必须覆盖无 hash 工具名、旧 `_xxxxxxxx`、legacy scaffold、空成功、thinking 和结构错配的原因。

## 限制

Git 历史只能证明实现与 tag 的时间关系，不能单独给出生产频率。生产 recurrence、版本分布和真实 request 链仍需通过只读 usage/debug evidence 复核；在没有对应时间窗数据前，不把某个 commit 写成频率上升的唯一因果结论。
