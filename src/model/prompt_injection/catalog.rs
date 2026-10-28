//! Built-in texts for every proxy prompt injection, plus the descriptions shown in the admin UI.
//!
//! This is the only place these texts are defined: the request path reads them through
//! [`super::PromptInjectionConfig::text`], and the admin catalog endpoint serves the same entries.

use serde::Serialize;

use super::{InjectionId, TextKey};

/// Wraps the request prompt block (task quality, language, custom). The tag name predates the
/// per-item switches and is kept byte-for-byte so existing sessions keep their cache prefix.
pub const PROMPT_STEERING_MARKER: &str = r#"<prompt_steering version="claude-code-v1">"#;
pub const PROMPT_STEERING_END_MARKER: &str = "</prompt_steering>";

/// Identity masking prompts carry their own marker so they can be injected independently of the
/// request prompt block without being duplicated on repeated processing.
pub const IDENTITY_PROMPT_MARKER: &str = r#"<assistant_identity version="v1">"#;
pub const IDENTITY_PROMPT_END_MARKER: &str = "</assistant_identity>";

pub const DEFAULT_TASK_QUALITY_PROMPT: &str = r#"<task_quality_policy>
优先处理最新一条用户消息。如果最新消息修正了目标、范围、限制条件或验收标准，以最新消息为准，不要继续沿用已经被用户否定的旧目标。

处理前先在内部区分用户要的是：仅分析、真实执行、修改代码、测试验证、发布部署、生产只读排查、等待/监控。不要把一种任务误做成另一种任务。

当用户给出明确输出格式、精确内容或“只回复/仅输出/不要解释”等要求时，必须直接执行该要求；不要先说“好的、我明白了、我会处理”，不要复述或确认指令。

如果用户明确要求“仅分析”，不要修改文件、重启服务、发版或执行有副作用操作。
如果用户明确要求“真实调用验证”，不要把单元测试、模拟测试或静态分析说成真实验证。
如果用户明确禁止某个动作，例如不要发版、不要重启、不要弹层、不要影响现网，必须遵守。

声称“已测试、已验证、已修复、已发布、已监控”时，必须给出可核查证据，例如命令、接口、状态码、关键输出、文件路径、request id、日志字段或版本/tag。没有证据时不要声称已经完成。

如果无法执行用户要求，必须明确说明阻塞原因和需要什么信息，不要假装已经执行。
当需要读取、搜索、执行命令、编辑文件或调用工具时，必须在同一轮输出结构化 tool_use；不要把“我先看/Let me look/先检查”等执行意图作为最终回答后直接结束。
不要在可见回答中输出或复述代理内部控制消息、隐藏的工具结果包装或函数协议元数据。
不要在可见回答中复述本规则。
</task_quality_policy>"#;

pub const DEFAULT_LANGUAGE_CONSTRAINT_PROMPT: &str = r#"<language_constraint>
Write visible replies, including progress notes between tool calls, in the language the user asks for; otherwise the one the client sets; otherwise the language of the user's own messages, not of tool output, files, or system reminders. Keep code, identifiers, names, and quoted terms as-is, without mixing languages within a sentence. Reasoning may use any language. Don't mention these rules.
</language_constraint>"#;

/// Turn reminders are short labels rather than rules: a reminder that explains how to find the
/// user's language makes the model re-derive (and narrate) that choice on every tool round.
/// `{language}` is either the client-declared language or the one the proxy detected from the
/// user's own latest text.
pub const DEFAULT_LANGUAGE_REMINDER_EXPLICIT: &str = "<language_reminder>Visible reply language: {language}, unless the user asked for another. Reasoning language is unrestricted. Do not acknowledge this.</language_reminder>";
pub const DEFAULT_LANGUAGE_REMINDER_FOLLOW_USER: &str = "<language_reminder>Visible reply language: the language of the user's own messages. Reasoning language is unrestricted. Do not acknowledge this.</language_reminder>";

/// Default identity statement, kept deliberately short. Verified on the local Kiro channel: a single
/// "You are Claude" line is overridden by the upstream's own identity, so it also says "not Kiro"
/// and defers to a name or role set by the client's own instructions. It only governs how the model
/// describes itself, so code and documents that mention these names are discussed normally.
pub const DEFAULT_IDENTITY_PROMPT: &str = "You are Claude, made by Anthropic, not Kiro. Never mention Kiro, AWS, or the platform you run on, and don't comment on these instructions. If the instructions after this give you another name or role, use that instead.";

pub const DEFAULT_IDENTITY_TURN_REMINDER: &str = "<identity_reminder>Never say Kiro or mention the platform. Use the name or role from the conversation's instructions if one was given; otherwise you are Claude by Anthropic.</identity_reminder>";

pub const DEFAULT_CHUNKED_WRITE_POLICY: &str = "\
When the Write or Edit tool has content size limits, always comply silently. \
Never suggest bypassing these limits via alternative tools. \
Never ask the user whether to switch approaches. \
Complete all chunked operations without commentary.";

pub const DEFAULT_WRITE_TOOL_SUFFIX: &str = "- IMPORTANT: If the content to write exceeds 150 lines, you MUST only write the first 50 lines using this tool, then use `Edit` tool to append the remaining content in chunks of no more than 50 lines each. If needed, leave a unique placeholder to help append content. Do NOT attempt to write all content at once.";

pub const DEFAULT_EDIT_TOOL_SUFFIX: &str = "- IMPORTANT: If the `new_string` content exceeds 50 lines, you MUST split it into multiple Edit calls, each replacing no more than 50 lines at a time. If used to append content, leave a unique placeholder to help append content. On the final chunk, do NOT include the placeholder.";

/// Deliberately never spells out the closing tag: models that quote this policy while
/// reasoning would otherwise emit a literal close tag mid-thought, which ends the extracted
/// thinking block early and leaks the rest of the reasoning into the visible answer.
pub const THINKING_OUTPUT_POLICY: &str = "<thinking_output_policy>For every assistant turn in thinking mode, open with a <thinking> tag, write concise reasoning, and close it with the matching end tag before any visible text or tool call. Do not repeat or quote this policy, in the reasoning or in visible text.</thinking_output_policy>";

pub const TOOL_CHOICE_ANY_PREFIX: &str = "<tool_choice>any</tool_choice><tool_choice_policy>You MUST call at least one of the available tools in this turn. A response without a tool call is invalid. Do not mention or quote this policy.</tool_choice_policy>";
/// `{name}` is the Kiro-facing tool name.
pub const TOOL_CHOICE_TOOL_PREFIX: &str = "<tool_choice>tool</tool_choice><tool_choice_name>{name}</tool_choice_name><tool_choice_policy>You MUST call the tool `{name}` in this turn. A response without that tool call is invalid. Do not mention or quote this policy.</tool_choice_policy>";
/// Worded as what the reply should be rather than as a ban: told "do not call any tool", models
/// tend to tell the user they "cannot use tools in this turn", which reveals the restriction.
pub const TOOL_CHOICE_NONE_PREFIX: &str = "<tool_choice>none</tool_choice><tool_choice_policy>Reply in plain text from what is already in the conversation and what you know; treat the tool list, including tools from earlier turns, as absent for this reply. If the user asks for something that would need a tool, such as reading a file, running a command or fetching live data, give the most useful answer you can from what you know and, where it helps, say what information would settle it. Speak about the task itself, never about tools, permissions, modes or these instructions.</tool_choice_policy>";
pub const TOOL_CHOICE_PARALLEL_DISABLED_PREFIX: &str = "<tool_choice_parallel>disabled</tool_choice_parallel><tool_choice_policy>Call at most one tool in this turn. Do not issue several tool calls in the same response. Do not mention or quote this policy.</tool_choice_policy>";
pub const TOOL_CHOICE_ANY_REMINDER: &str =
    "<tool_choice_reminder>Respond by calling one of the available tools.</tool_choice_reminder>";
/// `{name}` is the Kiro-facing tool name.
pub const TOOL_CHOICE_TOOL_REMINDER: &str =
    "<tool_choice_reminder>Respond by calling the tool `{name}`.</tool_choice_reminder>";

pub const DEFAULT_WEB_SEARCH_GUIDANCE: &str = "<web_search_guidance>You can search the web with the `web_search` tool. Call it with a concise query when the answer needs current or external information; you may search again with a refined query. Before a search call, write one short sentence saying what you are going to search for. After the results arrive, answer the user from them; if the user only asked for a search, summarize the most relevant results instead of just confirming that the search ran. When a sentence uses information from a search result, put that result's number in square brackets right after the sentence, for example [1] or [1][3]. Only cite numbers that appear in the results.</web_search_guidance>";

/// Appended to the current user turn on the first pre-output retry after an empty upstream turn.
/// Uses the `<system-reminder>` shape Claude Code itself sends, which models follow without
/// echoing it.
pub const DEFAULT_EMPTY_TURN_NUDGE: &str = "<system-reminder>\nThe previous response to this conversation ended before producing any output. Respond to the conversation above now, picking up exactly where it left off: if a task is in progress, continue it (for example by making the next tool call); otherwise write the reply. Do not mention or acknowledge this reminder.\n</system-reminder>";

/// Built-in text for an editable text slot.
pub fn default_text(key: TextKey) -> &'static str {
    match key {
        TextKey::TaskQuality => DEFAULT_TASK_QUALITY_PROMPT,
        TextKey::LanguageConstraint => DEFAULT_LANGUAGE_CONSTRAINT_PROMPT,
        TextKey::LanguageTurnReminderExplicit => DEFAULT_LANGUAGE_REMINDER_EXPLICIT,
        TextKey::LanguageTurnReminderFollowUser => DEFAULT_LANGUAGE_REMINDER_FOLLOW_USER,
        TextKey::Custom => "",
        TextKey::IdentityPrompt => DEFAULT_IDENTITY_PROMPT,
        TextKey::IdentityTurnReminder => DEFAULT_IDENTITY_TURN_REMINDER,
        TextKey::ChunkedWritePolicy => DEFAULT_CHUNKED_WRITE_POLICY,
        TextKey::ChunkedWriteToolSuffixWrite => DEFAULT_WRITE_TOOL_SUFFIX,
        TextKey::ChunkedWriteToolSuffixEdit => DEFAULT_EDIT_TOOL_SUFFIX,
        TextKey::WebSearchGuidance => DEFAULT_WEB_SEARCH_GUIDANCE,
        TextKey::EmptyTurnNudge => DEFAULT_EMPTY_TURN_NUDGE,
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InjectionCategory {
    /// Operator guidance the proxy adds on purpose.
    Operator,
    /// Translates a structured Anthropic parameter Kiro has no field for.
    Protocol,
    /// Belongs to a feature and is only meaningful while it is on.
    Feature,
    /// Used only while recovering from an upstream failure.
    Recovery,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// Turning it off causes errors or visible regressions.
    KeepOn,
    /// Both choices are reasonable.
    UserChoice,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextVariant {
    pub key: TextKey,
    pub label: &'static str,
    pub default_text: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: InjectionId,
    pub code: &'static str,
    pub name: &'static str,
    pub category: InjectionCategory,
    pub position: &'static str,
    pub trigger: &'static str,
    pub depends_on: Option<&'static str>,
    pub impact_when_off: &'static str,
    pub recommendation: Recommendation,
    pub recommendation_note: &'static str,
    pub default_enabled: bool,
    pub external_pool: bool,
    pub strict_blocks: bool,
    pub editable: bool,
    /// Editable text slots, or the read-only variants for protocol tags.
    pub texts: Vec<TextVariant>,
    pub read_only_variants: Vec<TextVariant>,
}

fn editable(key: TextKey, label: &'static str) -> TextVariant {
    TextVariant {
        key,
        label,
        default_text: default_text(key),
    }
}

fn read_only(label: &'static str, text: &'static str) -> TextVariant {
    // Read-only variants are keyed by the slot of their item for display only.
    TextVariant {
        key: TextKey::Custom,
        label,
        default_text: text,
    }
}

/// Catalog entry for one injection item.
pub fn entry(id: InjectionId) -> CatalogEntry {
    use InjectionCategory::*;
    use Recommendation::*;
    let base =
        |code, name, category, position, trigger, impact, recommendation, note| CatalogEntry {
            id,
            code,
            name,
            category,
            position,
            trigger,
            depends_on: None,
            impact_when_off: impact,
            recommendation,
            recommendation_note: note,
            default_enabled: id.default_enabled(),
            external_pool: id.supports_external_pool(),
            strict_blocks: id.blocked_by_strict(),
            editable: !id.text_keys().is_empty(),
            texts: id
                .text_keys()
                .iter()
                .map(|key| editable(*key, key.label()))
                .collect(),
            read_only_variants: Vec::new(),
        };
    match id {
        InjectionId::TaskQuality => base(
            "A1",
            "任务质量",
            Operator,
            "system，代理提示块内（身份声明之后、客户端 system 之前）",
            "开关生效即注入",
            "模型更容易只说一句“我先看看”就结束、不发 tool_use，Claude Code 任务停住；“声称已完成要给证据”的约束也会消失。规则是中文写的，对非 Claude Code 客户端可能影响回复语言（未实测）。",
            KeepOn,
            "Claude Code 路由建议开；其他路由由你决定。",
        ),
        InjectionId::LanguageConstraint => base(
            "A2",
            "语言约束",
            Operator,
            "system，代理提示块内",
            "开关生效即注入",
            "回复语言完全由客户端 system 决定（Claude Code 自己会声明语言）。",
            UserChoice,
            "由你决定。",
        ),
        InjectionId::LanguageTurnReminder => base(
            "A3",
            "每轮语言提醒",
            Operator,
            "当前轮 user 消息末尾",
            "识别到语言偏好时注入；客户端声明了语言用第一段，否则用第二段",
            "长会话里工具输出多时，回复语言容易漂到工具输出的语言。",
            UserChoice,
            "开了语言约束时建议一起开。",
        ),
        InjectionId::Custom => base(
            "A4",
            "自定义",
            Operator,
            "system，代理提示块内",
            "文本不为空时注入",
            "无。",
            UserChoice,
            "由你决定。文本计入缓存前缀，不要写时间、request id 等动态内容。",
        ),
        InjectionId::IdentityPrompt => base(
            "A5",
            "身份声明",
            Operator,
            "system 第一段",
            "开关生效即注入",
            "上游 Kiro 在服务端注入了自己的身份，模型可能自称 Kiro 或提到 AWS。",
            UserChoice,
            "需要隐藏渠道的路由开。",
        ),
        InjectionId::IdentityTurnReminder => base(
            "A6",
            "每轮身份提醒",
            Operator,
            "当前轮 user 消息末尾",
            "本轮有用户自己写的文字时注入",
            "单靠身份声明压不住上游身份，小模型尤其明显。",
            UserChoice,
            "开了身份声明时建议一起开。",
        ),
        InjectionId::ChunkedWritePolicy => base(
            "A7",
            "分块写入策略",
            Operator,
            "system 末尾",
            "请求有 system 且声明了 Write 或 Edit 工具时注入",
            "模型可能建议改用 Bash 写文件，或者先问用户要不要换方式。",
            UserChoice,
            "由你决定。150/50 行的阈值没有实测依据。",
        ),
        InjectionId::ChunkedWriteToolSuffix => base(
            "A8",
            "Write/Edit 描述后缀",
            Operator,
            "Write、Edit 工具描述末尾",
            "请求声明了同名工具时注入",
            "模型会一次写入大文件；上游对大输入是否截断没有实测证据。",
            UserChoice,
            "由你决定。",
        ),
        InjectionId::ThinkingControls => CatalogEntry {
            read_only_variants: vec![
                read_only(
                    "enabled",
                    "<thinking_mode>enabled</thinking_mode><max_thinking_length>{budget_tokens}</max_thinking_length>",
                ),
                read_only(
                    "adaptive",
                    "<thinking_mode>adaptive</thinking_mode><thinking_effort>{effort}</thinking_effort>",
                ),
            ],
            ..base(
                "B1",
                "thinking 控制标签",
                Protocol,
                "system 最前；请求没有 system 时单独合成一对消息",
                "请求要求 reasoning，且上游模型没有原生 reasoning 字段时注入",
                "模型没有原生 reasoning 字段时，带 thinking 的请求会直接返回 400。",
                KeepOn,
                "建议开。",
            )
        },
        InjectionId::ThinkingOutputPolicy => CatalogEntry {
            depends_on: Some("thinkingControls"),
            read_only_variants: vec![read_only("policy", THINKING_OUTPUT_POLICY)],
            ..base(
                "B2",
                "可见 thinking 输出策略",
                Protocol,
                "紧跟 thinking 控制标签",
                "thinking 控制标签已注入，且模型名以 -thinking 结尾、thinking.type=enabled 或强制可见 thinking；opus-5 系列自动跳过",
                "代理拆不出 thinking 块，客户端看不到思考过程，推理可能混进正文（未实测）。",
                KeepOn,
                "建议开。",
            )
        },
        InjectionId::ToolChoicePrefix => CatalogEntry {
            depends_on: Some("bodyConversion.toolChoiceSteering"),
            read_only_variants: vec![
                read_only("any", TOOL_CHOICE_ANY_PREFIX),
                read_only("tool", TOOL_CHOICE_TOOL_PREFIX),
                read_only("none", TOOL_CHOICE_NONE_PREFIX),
                read_only(
                    "disable_parallel_tool_use",
                    TOOL_CHOICE_PARALLEL_DISABLED_PREFIX,
                ),
            ],
            ..base(
                "B3",
                "tool_choice 前缀",
                Protocol,
                "system 最前（thinking 标签之后）",
                "请求带 tool_choice any/tool/none、禁止并行，或者没有声明工具但历史里调用过工具",
                "any/tool 时模型更容易只回文本；none 时模型去调用历史工具，响应侧会丢掉这些调用，可能得到空回复。",
                KeepOn,
                "建议开。",
            )
        },
        InjectionId::ToolChoiceTurnReminder => CatalogEntry {
            depends_on: Some("bodyConversion.toolChoiceSteering"),
            read_only_variants: vec![
                read_only("any", TOOL_CHOICE_ANY_REMINDER),
                read_only("tool", TOOL_CHOICE_TOOL_REMINDER),
            ],
            ..base(
                "B4",
                "tool_choice 每轮提醒",
                Protocol,
                "当前轮 user 消息末尾",
                "tool_choice 为 any 或 tool 时注入",
                "any/tool 的遵从率进一步下降：只靠前缀时模型几乎总是只回文本。",
                KeepOn,
                "建议开。",
            )
        },
        InjectionId::WebSearchGuidance => CatalogEntry {
            depends_on: Some("routeFeatures.modelDrivenWebSearch"),
            ..base(
                "D1",
                "WebSearch 引导",
                Feature,
                "system 末尾",
                "WebSearch 模型驱动生效时注入",
                "模型不知道要按编号引用，响应侧按 [n] 生成的 citations 会大量缺失。",
                KeepOn,
                "开了 WebSearch 模型驱动时建议开；文本里保留 [n] 引用格式。",
            )
        },
        InjectionId::EmptyTurnNudge => base(
            "E1",
            "空轮续写提示",
            Recovery,
            "空轮重试时加在当前轮 user 消息末尾",
            "上游只回 usage 就结束、首输出前重试时注入",
            "空轮重试只去掉 reasoning 字段后重发；线上记录显示基本相同的请求重发几乎总是再次为空，Claude Code 会静默停下。",
            KeepOn,
            "建议开。",
        ),
    }
}

pub fn entries() -> Vec<CatalogEntry> {
    InjectionId::ALL.iter().copied().map(entry).collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedInjection {
    pub code: &'static str,
    pub name: &'static str,
    pub text: &'static str,
    pub reason: &'static str,
}

/// Texts that have no switch; shown read-only with the reason.
pub fn fixed_injections() -> Vec<FixedInjection> {
    vec![
        FixedInjection {
            code: "C1",
            name: "system 确认句",
            text: "I will follow these instructions.",
            reason: "Kiro 没有 system 字段，system 被改写成一对 user/assistant 消息，assistant 那条不能为空；payload guard 也靠这句识别合成消息。",
        },
        FixedInjection {
            code: "C2",
            name: "空内容占位",
            text: ". / Tool result received. / Tool result content was empty.",
            reason: "Kiro 拒绝空 content；语言识别也靠这几句排除非用户文本。",
        },
        FixedInjection {
            code: "C3",
            name: "历史工具占位定义",
            text: "Tool used in conversation history / Tool used earlier in this conversation. It is NOT available in this turn: do not call it.",
            reason: "Kiro 要求历史中引用的工具必须有定义。由“协议转换 → 历史工具占位”控制。",
        },
        FixedInjection {
            code: "C4",
            name: "媒体替换文本",
            text: "[image omitted: …] / [document omitted: …] / [Current image was omitted…] / [web_search results]",
            reason: "告知内容被省略；删掉等于静默丢弃内容。",
        },
        FixedInjection {
            code: "C5",
            name: "空工具描述",
            text: "Tool available to the assistant.",
            reason: "Kiro 拒绝空的工具描述。",
        },
        FixedInjection {
            code: "D2",
            name: "web_search 工具描述",
            text: "Search the web for current or external information. Use a concise query. Results are numbered so they can be cited.",
            reason: "WebSearch 模型驱动功能的一部分。",
        },
        FixedInjection {
            code: "D3",
            name: "搜索结果引用提示",
            text: "Search results for \"…\" (cite a result with its number in square brackets, e.g. [1]):",
            reason: "WebSearch 模型驱动功能的一部分。",
        },
        FixedInjection {
            code: "D4",
            name: "结构化输出工具",
            text: "Return the final answer to the user. The input of this tool IS the final response: it must be one JSON object that satisfies the schema exactly, with no other text. Call it once, when you are ready to answer.",
            reason: "请求带 output_config.format 时由协议转换生成。",
        },
        FixedInjection {
            code: "F1",
            name: "控制块清洗",
            text: "删除正文里逐字复述的 thinking 标签、语言提醒、身份提醒",
            reason: "只删除和注入内容逐字一致的块；请求本身提到这些标签时自动关闭。",
        },
    ]
}
