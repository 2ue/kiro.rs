# 抓包证据保留策略

更新时间：2026-11-07

## 当前状态

所有 Round 55–63 抓包、59138 生产参照抓包和 Round 63 原始服务端会话继续保留。`stream_structure`、`websearch`、`signature_proto` 尚未解决，原始事件仍可能需要按请求 ID 回放，当前不清理任何尚未完成关联的源文件。

## 清理条件

一个轮次只有同时满足以下条件，才允许清理重复源文件：

1. 评分结果和请求级矩阵已经写入 `results/`、`rounds/` 与 `analysis/`；
2. 关键字段统计、失败请求 ID、状态机结论和版本边界已固定；
3. 后续假设不再需要该轮的原始正文；
4. 已保留一个可恢复的原始归档（例如 `capture.zip`）和一份脱敏统计；
5. 删除对象只是重复展开目录或重复压缩包，不删除唯一原始事件源；
6. 清理前先记录文件路径、大小和所属轮次，清理后复核文档链接仍指向保留归档。

## 不允许清理

- 生产 59138 原始抓包；
- 任何含 WebSearch opaque 字段的唯一抓包；
- 含 EOF/COMPLETED、签名、重试链或上游错误样本的唯一抓包；
- 尚未完成 CCTest 请求关联的轮次；
- 仅因为磁盘增长就实时删除的文件。

## 可清理对象与固定条件

证据链固定后可以清理积累的重复源文件，但不能实时清理。只有同时满足以下条件，才允许删除本地文件：

1. 评分结果、请求级关联、失败项统计和结论已经写入 `results/`、`rounds/` 与 `analysis/`；
2. 至少保留一个可恢复的 `capture.zip`、一个 `manifest.json` 和可校验哈希；
3. 待删除对象不是唯一的 `events.jsonl`，也不是唯一能恢复正文的归档；
4. 删除对象只是重复展开目录、重复压缩包或零事件空会话，不含尚未固定的失败样本；
5. 清理前登记路径、大小、哈希、所属轮次和恢复来源，清理后复核剩余归档可读。

59138 生产原始抓包继续只读保留，不参与本地清理。当前三个长期失败项尚未固定根因，因此 Round 76、80、82、83、84 的完整原始事件和归档均保留。

## Round 63

Round 63 同时存在服务端会话归档和分析展开目录。由于提示词来源、上游 EOF 和三项失败评分仍需继续关联，两个目录暂时都保留，后续固定证据后再按上述规则去除重复副本。

## Round 64–67

- Round 64 没有启动新抓包，只使用已有证据做稳定复测；结果文件和轮次记录已固定。
- Round 65 原始事件、manifest 和 `/tmp/kirotest-dev/artifacts/round-65-capture.zip` 保留。
- Round 66 原始事件、manifest 和 `/tmp/kirotest-dev/artifacts/round-66-capture.zip` 保留。
- Round 67 是提示词 A/B 失败样本，包含本地 400 的请求级证据；原始事件、manifest 和 `/tmp/kirotest-dev/artifacts/round-67-capture.zip` 保留。
- Round 68 是当前运行时配置下的 WebSearch 回灌样本；`server_tool_use`、MCP 200、`tool_use_id` 配对、citation、最终回答和终止事件均已固定，但 `websearch=0` 的 opaque 字段根因仍未固定。原始事件、manifest 和 `/tmp/kirotest-dev/artifacts/round-68-capture.zip` 保留。

这三轮都仍支撑未解决的流结构、签名和 WebSearch 分析，因此没有删除任何原始源文件，也没有实时清理抓包目录。

Round 68 同样没有删除原始文件。当前失败项仍需要原始 WebSearch 回灌顺序、结果字段和上游请求体，不能只依赖摘要。

## Round 69–71

- Round 69 原始事件、ZIP、manifest 与结果文件全部保留。该轮包含 402 账号切换、WebSearch、签名和完整流结构，不能删除唯一源文件。
- Round 70 原始事件、ZIP、manifest 与结果文件全部保留。该轮是 caller 的部分对照，必须与 Round 71 的真正流式对照并列保存。
- Round 71 原始事件、ZIP、manifest 与结果文件全部保留。该轮固定了“流式缺失 caller 仍得 82/110”的证据，且 WebSearch 失败项仍需原始 opaque 字段复核。

截至 Round 71，没有删除任何原始抓包。后续只有在对应失败项、请求 ID、字段统计和结论均已固定，并且保留可恢复 ZIP 后，才可清理重复展开副本；生产 59138 的约 4 GiB 原始文件继续永久保留。

- Round 72 原始事件、ZIP、manifest 与结果文件全部保留。该轮固定了 WebSearch 两个生成块显式 `caller=direct` 后仍为 `websearch=0`，且包含 402 切换与完整 block 统计，不能删除唯一源文件。

## Round 73–76

- Round 73–74 的结果文件保留；Round 73 使用错误模型，作为排除条件证据，不能覆盖正确模型基线。
- Round 75 的结果和定向协议测试记录保留；其 thinking 起始块字段修复虽未改变评分，但仍是公开协议证据。
- Round 76 的 `events.jsonl`、`capture.zip`、`manifest.json`、结果文件和逐请求矩阵全部保留。该轮含 13 个真实 signature 对照和 WebSearch opaque 字段，失败根因尚未固定。
- 不实时删除新抓包；只有证据链固定且存在可恢复归档后，才清理重复展开副本。59138 生产原始抓包继续只读保留。

## Round 77–78

Round 77/78 的结果文件和实验抓包继续保留。伪 opaque envelope 已回退，但两轮仍用于证明该变量没有让 `websearch` 共变；不删除原始 JSONL、ZIP 或 manifest。

## Round 79–80

- Round 79 结果文件保留；本轮确认当前工作树仍为 `82/110`，没有新增公开协议异常。
- Round 80 原始 `events.jsonl`、`capture.zip`、`manifest.json`、结果文件和请求矩阵全部保留。该轮固定了 WebSearch 的完整 MCP 回灌链路，同时保留 `encrypted_content` 明文和 `encrypted_index` 可逆 Base64 的唯一字段证据。
- Round 80 还包含上游 `<system-reminder>` / `<thinking_mode>` marker、真实 signature、EOF 无 `COMPLETED` 和 WebSearch citation，仍支撑三个未解决评分项，不能清理。
- Round 83 新增 402 换号成功链和完整请求级 retry 证据；由于 429/500 遗留失败尚未复现，`events.jsonl`、`capture.zip`、`manifest.json` 全部保留，不能用本轮成功样本替代旧生产失败摘要。
- 只有在官方 opaque fixture、CCTest 失败样本和后续多轮回传证据均固定后，才可考虑删除重复展开副本；生产 59138 原始抓包继续只读永久保留。

## Round 84

Round 84 是 `thinkingOutputPolicy` 非触发边界实验。该开关在 Opus 5.5 tune 路由上未实际注入，完整结果仍为 `82/110`；本轮原始 `events.jsonl`、`capture.zip`、`manifest.json` 仍支撑提示词触发条件与评分项的关联分析，全部保留：

```text
logs/llm-capture/capture-20261107T211537Z-d5880da8/
```

本轮前后两次误启动产生了两个零事件空会话：

```text
capture-20261107T211436Z-9501b0c0
capture-20261107T211519Z-2ddd736f
```

它们均为 `events.jsonl` 0 字节、无请求、无响应、无评分数据，且不是任何证据的唯一来源。删除前已登记哈希；这类空重复源文件属于已固定的可清理对象。保留 Round 84 正式归档，不清理任何含请求正文的文件。

清理登记（本地开发抓包目录）：

| 会话 | `events.jsonl` SHA-256 | `capture.zip` SHA-256 | 处理 |
|---|---|---|---|
| `capture-20261107T211436Z-9501b0c0` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` | `535157f073772c95c583b0509218d428bf0449589ad16746d5ef359a655d6744` | 零事件空会话，删除重复源文件 |
| `capture-20261107T211519Z-2ddd736f` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` | `322b958e890bf0797ef3070d02a31130bd57c2647fe80d5537d5feb37c6066b3` | 零事件空会话，删除重复源文件 |

删除对象不影响 Round 84 正式归档；正式归档的哈希见 Round 84 manifest 和本地文件清单。

## Round 87

Round 87 的 `websearch=0`、`stream_structure=5`、`signature_proto=5` 仍未固定根因，且该轮包含最新高余额账号、真实签名、EOF 终止、提示词 marker 和 WebSearch opaque 字段样本。唯一原始文件全部保留：

```text
logs/llm-capture/capture-20261108T025405Z-29d0590d/
```

登记哈希：

```text
events.jsonl  4a588bcacdd83d2c5c1275a384da1e61c6b31660daf67a7f63556791ed8ead9b
capture.zip   90f7155e3e75ddfd33b57fc270c9a53d0e0ae05161fc5520e46a03ff9ef8da8e
manifest.json 69140c99d312bca6f6d188a411d2b725ac6686ee7caac1ceca03193f6d2bdbc0
```

本轮没有零事件空会话或可清理重复展开副本；不删除、不移动、不覆盖任何源文件。59138 生产原始抓包继续只读永久保留。

## Round 81

Round 81 结果文件保留。该轮只验证 WebSearch opaque 字段的“真实值存在时原样透传”边界；Kiro MCP 实际没有提供 opaque 值，因此没有新增可替代的唯一字段证据，也没有删除 Round 80 原始抓包。

## Round 82

Round 82 原始 `events.jsonl`、`capture.zip`、`manifest.json` 与结果文件全部保留：

```text
logs/llm-capture/capture-20261107T204110Z-fdbcf638/
```

本轮包含 synthetic reasoning 边界修复后的首次完整评分、一次上游 402 额度耗尽切换、WebSearch 回灌、真实 signature 和客户端提示 marker。`stream_structure`、`websearch`、`signature_proto` 仍未解决，因此不能清理该轮唯一源文件。

已固定的重复统计可以写入 Round 82 文档，但不删除原始 JSONL、ZIP 或 manifest。生产 59138 抓包继续只读永久保留。

## Round 88

Round 88 原始事件、ZIP、manifest 和结果文件全部保留：

```text
logs/llm-capture/capture-20261108T031042Z-6baf9b7a/
```

登记哈希：

```text
events.jsonl  2910d12b67b4270c082c36c35e8f52760dd8beec2dfe05ff08ec98e7071e5d94
capture.zip   7ffa4f9bf836b0517efb122d6030ecbbfe38a5260ddb039928d79f9ee5c4413
manifest.json fb85ef9ebdadac132ce955f0df7a18cd7b74a836045ad9dff95fdac86f44322d
```

本轮包含非流 reasoning/空 thinking block 的新对照、WebSearch opaque 字段和 402/429 成功换号链，仍支撑未解决的评分问题；没有零事件空会话或可清理重复展开副本，不删除、不移动、不覆盖任何源文件。59138 生产原始抓包继续只读永久保留。

## Round 89

Round 89 原始事件、ZIP、manifest 和结果文件全部保留：

```text
logs/llm-capture/capture-20261108T032943Z-7b2c6207/
```

登记哈希：

```text
events.jsonl  bfbff84b703d52e247a99de09d0c7388e23b7a120d525778aec07633b09e534e
capture.zip   0131816ca5238d7bdd1afb971c9441d96ee7092f1f38b29c235ad065645fb1cd
manifest.json d4fb263ff97ac6477d89c29f05fe54940b8d59b1ca6c532c39ade95024ae2f50
```

本轮包含 Round 88 non_stream=0 的反向对照、WebSearch opaque 字段和 402 换号链；失败项仍未固定根因，不删除、不移动、不覆盖任何源文件。59138 生产原始抓包继续只读永久保留。

## Round 90

Round 90 原始事件、ZIP、manifest 和结果文件全部保留：

```text
logs/llm-capture/capture-20261108T033805Z-6e3de299/
```

登记哈希：

```text
events.jsonl  e105efe1f5ecb08f8c58e2fcb33f32211ad22e0caa7a4101fc77fc6bf8094265
capture.zip   65f3cbeda59bd97e1155b8b79019c87d9cea20f766c89202eb1dc4398ceb2dfa
manifest.json 5bee2faee6d43876fce8972153a747257965ac7087d0065ee5a4963e3463217d
```

本轮含 11 次 429→200 成功恢复链、非流对照和 WebSearch opaque 字段，仍支撑未解决评分问题；不删除、不移动、不覆盖任何源文件。59138 生产原始抓包继续只读永久保留。

## Round 91–95

Round 91–95 的失败项仍未固定根因，唯一原始 `events.jsonl`、`capture.zip`、`manifest.json`、结果文件和请求矩阵全部保留：

```text
logs/llm-capture/capture-20261108T034657Z-78bc03f4/
logs/llm-capture/capture-20261108T035947Z-629d5eb9/
logs/llm-capture/capture-20261108T040518Z-8e0b65a1/
logs/llm-capture/capture-20261108T041355Z-653a74d7/
logs/llm-capture/capture-20261108T042226Z-cb0abf9a/
logs/llm-capture/capture-20261108T043015Z-b377f71d/
logs/llm-capture/capture-20261108T043944Z-cc11fd9f/
```

Round 94 是 `thinkingTrigger=always` 运行时 A/B，Round 95 是恢复 `off` 后的完整复测。Round 95 的哈希登记如下：

```text
events.jsonl  e31b53ca276664b4b0667094a7e117f0773b0943ba867c09404a5dbb905b03ff
capture.zip   2db8ca1ecad82f730095425312bacb9f6c61c86e7a93e6660dabb778215ba70a
manifest.json a43a629dce8ab54ba58732f6bd7a3e9033191333c2cc1c8c0232bb0553238863
```

Round 96 哈希：

```text
events.jsonl  f0f1434aa61a0270aac53c60389793e8e4b25f7a952bbad8562af86871aaf585
capture.zip   6f94ee9d3ec7d8b0e33c8dac11ef034b8f3f2b3aee39c58a91d5226aec0f35e3
manifest.json 5326511f765f72d0498ffec77780a2e47b87a970d0b1e40654810dc076e81a0e
```

Round 97 哈希：

```text
events.jsonl  da3a8343f099165c6dc6272b40cb886c47677057d021cc9001cf1e4a27ebc02d
capture.zip   393845d29d89d7a60199bc6b4289c4284148ae428e1bb2bb651b636c8551a500
manifest.json 2b593f7041f5d88647723a207b28a40cb358b3de287c0bfa5d2be3ab5438b416
```

这些轮次包含非流 reasoning 对照、真实 Kiro signature、EOF 终止、WebSearch opaque 字段和提示词运行时 A/B；没有零事件空会话或已登记可清理的重复展开副本，不删除、不移动、不覆盖任何源文件。59138 生产原始抓包继续只读永久保留。

## Round 98

Round 98 的唯一原始抓包、归档和清单全部保留：

```text
logs/llm-capture/capture-20261108T044554Z-59d09b8f/
```

登记哈希：

```text
events.jsonl  f797d40f2c926b836bc31e51331b016e2671a5dceb55f90230bf66ccbc46e96a
capture.zip   cf69d4d16247eb8b6ce3c3aca744dfc412b8ef5d7a835e98fdf034a7d0a41a92
manifest.json fe3973d40da80922f24fb7893d03a407217dc19b25ebe64557f556637cd18ab1
```

本轮包含 402 额度耗尽后的账号换号成功链、真实 signature、EOF 推断和 WebSearch 回灌证据；失败项仍未固定根因，不能用后续轮次替代本轮唯一源文件。没有零事件空会话或已登记可清理的重复展开副本，不删除、不移动、不覆盖 `events.jsonl`、`capture.zip` 或 `manifest.json`。59138 生产原始抓包继续只读永久保留。

## Round 99

Round 99 的唯一原始抓包、归档和清单全部保留：

```text
logs/llm-capture/capture-20261108T045852Z-e9969a35/
```

登记哈希：

```text
events.jsonl  45f52d27f0ac008c1703cf755b102482166eb528699bb8a4fb53aa5e3c308e1f
capture.zip   94903fe049e0801210bc58fae7b1cc3d1788faf8267bf174adb2791b85652cf5
manifest.json 4c9b1d6e4c68dac62d78325ca36e646c05a55ff87eadb84a692c805b42b7c510
```

本轮含未声明 thinking 的 reasoning 隐藏对照、402→200 换号链、真实 signature、EOF 推断和 WebSearch 回灌证据；失败项仍未固定根因，不删除、不移动、不覆盖唯一 `events.jsonl`、`capture.zip` 或 `manifest.json`。59138 生产原始抓包继续只读永久保留。

## Round 100

Round 100 的唯一原始抓包、归档和清单全部保留：

```text
logs/llm-capture/capture-20261108T050827Z-cf5654a2/
```

登记哈希：

```text
events.jsonl  dc380aed8e73921fa4ac8af9bf124ddc601edca65161a232578ab116d884da08
capture.zip   d7f22a0121e0025f401437432a8640e7224133daf5d277ba5083d663ccdd7f0f
manifest.json 4d35fa2f6648d27eaa7ef09f3912ee44fbcce2fedcace9e54d48ebdfec8eaf5f
```

本轮没有零事件空会话或可清理重复副本。所有唯一原始文件继续保留；59138 生产原始抓包继续只读永久保留。

## Round 101

Round 101 的唯一原始抓包、归档和清单全部保留：

```text
logs/llm-capture/capture-20261108T051857Z-8136f530/
```

登记哈希：

```text
events.jsonl  e024ac0d8b79dd4ade94cba107b41c1534067fbb341353fbf1cb7f3313f02caa
capture.zip   f94ed7aa04cb1d4c5759eaa6636edefb8e10c37e3721a0b45230798b40b47ff5
manifest.json e1f9963ea81e357a8bfdab595a993423af361cb488c90a40daf3f039d1a9c3ea
```

本轮含 429→200 重试恢复链、真实 signature、EOF 推断和 WebSearch 回灌证据。失败项仍未固定根因，不删除、不移动、不覆盖唯一原始文件；59138 生产原始抓包继续只读永久保留。

## Round 102

本轮唯一原始证据保留在：

```text
logs/llm-capture/capture-20261108T052832Z-9611b44d/events.jsonl
logs/llm-capture/capture-20261108T052832Z-9611b44d/capture.zip
logs/llm-capture/capture-20261108T052832Z-9611b44d/manifest.json
```

`events.jsonl`、`capture.zip`、`manifest.json` 的 SHA-256 分别为：

```text
32ab850237d245ba30f2b939e33bca03d5a4935cbb97e76455df3141e3fca005
538044e311756d96f253f0173fb1819acd438f621335ddd764a8a22b5c680864
185349edd9973c450418281236969e8e53e6dc089e94713967246c5d54fa5eb4
```

本轮没有清理任何原始抓包；结果 JSON 和 request matrix 也保留。


## Round 103

Round 103 的唯一原始抓包、归档和清单全部保留：

```text
logs/llm-capture/capture-20261108T054953Z-fd2ddefb/
```

登记哈希：

```text
events.jsonl  72e35a04aae83ac4e6c96fc0005e1c9dadd120242f3258aa4dac0250d0fb856
capture.zip   f6ed91c6c175dd1eabe3bd8584c3264d05b82bd5535c04b5ad5b6eae35c1f925
manifest.json b72b3464779d55b7c912b8afea02f3fe57f47a49f538f1a7061d8dd7a1950980
```

本轮含 402→200 换号链、真实 signature、EOF 推断和 WebSearch 回灌证据；三个长期失败项仍未固定根因，不删除、不移动、不覆盖唯一 `events.jsonl`、`capture.zip` 或 `manifest.json`。Round 103 的结果和请求矩阵同步保留；59138 生产原始抓包继续只读永久保留。

Round 103 启动前有一次环境变量加载失败，产生零事件空会话：

```text
logs/llm-capture/capture-20261108T054912Z-fa863f63/
```

该目录的 `events.jsonl` 为 0 字节，未包含请求、响应或凭证；`capture.zip` 和 `manifest.json` 仍暂时保留，待后续统一清场时按零事件重复源文件规则处理。当前不实时删除。
