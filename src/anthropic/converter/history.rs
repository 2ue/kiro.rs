//! Conversation history construction for Anthropic -> Kiro conversion.

use std::collections::{HashMap, HashSet};

use crate::anthropic::types::{
    ContentBlock, Message as AnthropicMessage, MessagesRequest, validate_redacted_thinking_data,
};
use crate::kiro::model::requests::conversation::{
    AssistantMessage, HistoryAssistantMessage, HistoryUserMessage, Message, ReasoningContent,
    UserInputMessageContext, UserMessage,
};
use crate::kiro::model::requests::tool::ToolUseEntry;
use crate::model::config::CompatProfile;

use super::content::{normalize_tool_use_input, process_message_content, sanitize_tool_use_id};
use super::thinking::{generate_thinking_prefix_for_model, has_thinking_tags};
use super::tools::{SYSTEM_CHUNKED_POLICY, generate_tool_choice_prefix, map_tool_name};
use super::{ConversionError, ConverterOptions, ProxyWarnings, TOOL_RESULTS_PROVIDED_PLACEHOLDER};
use crate::anthropic::transcript_sanitizer::ToolTranscriptSanitizer;

/// 构建历史消息
///
/// # Arguments
/// * `req` - 原始请求，用于读取 `system`、`thinking` 等配置字段
/// * `messages` - 经过 prefill 预处理的消息切片，末尾必定是 user 消息。
///   注意：该切片与 `req.messages` 可能不同（prefill 时会截断末尾的 assistant 消息），
///   调用方应始终使用此参数而非 `req.messages`。
/// * `model_id` - 已映射的 Kiro 模型 ID
pub(super) fn build_history(
    req: &MessagesRequest,
    messages: &[AnthropicMessage],
    model_id: &str,
    tool_name_map: &mut HashMap<String, String>,
    warnings: &mut ProxyWarnings,
    options: ConverterOptions,
) -> Result<Vec<Message>, ConversionError> {
    let mut history = Vec::new();
    let mut known_tool_names = req
        .tools
        .iter()
        .flatten()
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    known_tool_names.extend(tool_name_map.keys().cloned());
    known_tool_names.extend(tool_name_map.values().cloned());
    for message in messages {
        let Some(blocks) = message.content.as_array() else {
            continue;
        };
        known_tool_names.extend(blocks.iter().filter_map(|block| {
            (block.get("type").and_then(|value| value.as_str()) == Some("tool_use"))
                .then(|| block.get("name").and_then(|value| value.as_str()))
                .flatten()
                .map(str::to_string)
        }));
    }

    // 生成thinking前缀（如果需要）
    let thinking_prefix = if options.inject_thinking_prefix() {
        generate_thinking_prefix_for_model(req, model_id, options.clone())?
    } else {
        None
    };
    let tool_choice_prefix = generate_tool_choice_prefix(req, options.clone());

    // 1. 处理系统消息
    if let Some(ref system) = req.system {
        let system_content: String = system
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        if !system_content.is_empty() {
            // 追加分块写入策略到系统消息
            let system_content = if options.inject_chunked_policy() {
                format!("{}\n{}", system_content, SYSTEM_CHUNKED_POLICY)
            } else {
                system_content
            };

            let system_content = if let Some(ref prefix) = tool_choice_prefix {
                format!("{}\n{}", prefix, system_content)
            } else {
                system_content
            };

            // 注入thinking标签到系统消息最前面（如果需要且不存在）
            let final_content = if let Some(ref prefix) = thinking_prefix {
                if !has_thinking_tags(&system_content) {
                    format!("{}\n{}", prefix, system_content)
                } else {
                    system_content
                }
            } else {
                system_content
            };

            // 系统消息作为 user + assistant 配对
            let user_msg = HistoryUserMessage::new(final_content, model_id);
            history.push(Message::User(user_msg));

            let assistant_msg = HistoryAssistantMessage::new("I will follow these instructions.");
            history.push(Message::Assistant(assistant_msg));
        }
    } else {
        let mut synthetic_prefixes = Vec::new();
        if let Some(prefix) = thinking_prefix {
            synthetic_prefixes.push(prefix);
        }
        if let Some(prefix) = tool_choice_prefix {
            synthetic_prefixes.push(prefix);
        }

        if !synthetic_prefixes.is_empty() {
            // 没有系统消息但有控制配置，插入新的系统消息
            let user_msg = HistoryUserMessage::new(synthetic_prefixes.join("\n"), model_id);
            history.push(Message::User(user_msg));

            let assistant_msg = HistoryAssistantMessage::new("I will follow these instructions.");
            history.push(Message::Assistant(assistant_msg));
        }
    }

    // 2. 处理常规消息历史
    // 最后一条消息作为 currentMessage，不加入历史
    // 经过 prefill 预处理后，messages 末尾必定是 user，故直接截掉最后一条即可
    let history_end_index = messages.len().saturating_sub(1);

    // 收集并配对消息
    let mut user_buffer: Vec<&AnthropicMessage> = Vec::new();
    let mut assistant_buffer: Vec<&AnthropicMessage> = Vec::new();

    for i in 0..history_end_index {
        let msg = &messages[i];

        if msg.role == "user" {
            // 先处理累积的 assistant 消息
            if !assistant_buffer.is_empty() {
                let merged = merge_assistant_messages_with_known_tools(
                    &assistant_buffer,
                    tool_name_map,
                    &known_tool_names,
                    warnings,
                    options.clone(),
                )?;
                history.push(Message::Assistant(merged));
                assistant_buffer.clear();
            }
            user_buffer.push(msg);
        } else if msg.role == "assistant" {
            // 先处理累积的 user 消息
            if !user_buffer.is_empty() {
                let merged_user = merge_user_messages(&user_buffer, model_id)?;
                history.push(Message::User(merged_user));
                user_buffer.clear();
            }
            // 累积 assistant 消息（支持连续多条）
            assistant_buffer.push(msg);
        }
    }

    // 处理末尾累积的 assistant 消息
    if !assistant_buffer.is_empty() {
        let merged = merge_assistant_messages_with_known_tools(
            &assistant_buffer,
            tool_name_map,
            &known_tool_names,
            warnings,
            options.clone(),
        )?;
        history.push(Message::Assistant(merged));
    }

    // 处理结尾的孤立 user 消息
    if !user_buffer.is_empty() {
        let merged_user = merge_user_messages(&user_buffer, model_id)?;
        history.push(Message::User(merged_user));

        // 自动配对一个 "OK" 的 assistant 响应
        let auto_assistant = HistoryAssistantMessage::new("OK");
        history.push(Message::Assistant(auto_assistant));
    }

    Ok(history)
}

/// 合并多个 user 消息
pub(super) fn merge_user_messages(
    messages: &[&AnthropicMessage],
    model_id: &str,
) -> Result<HistoryUserMessage, ConversionError> {
    let mut content_parts = Vec::new();
    let mut all_images = Vec::new();
    let mut all_tool_results = Vec::new();

    for msg in messages {
        let (text, images, tool_results) = process_message_content(&msg.content)?;
        if !text.is_empty() {
            content_parts.push(text);
        }
        all_images.extend(images);
        all_tool_results.extend(tool_results);
    }

    let mut content = content_parts.join("\n");
    if content.trim().is_empty() && !all_tool_results.is_empty() {
        content = TOOL_RESULTS_PROVIDED_PLACEHOLDER.to_string();
    }
    // 保留文本内容，即使有工具结果也不丢弃用户文本
    let mut user_msg = UserMessage::new(&content, model_id);

    if !all_images.is_empty() {
        user_msg = user_msg.with_images(all_images);
    }
    if !all_tool_results.is_empty() {
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(all_tool_results);
        user_msg = user_msg.with_context(ctx);
    }

    Ok(HistoryUserMessage {
        user_input_message: user_msg,
    })
}

/// 转换 assistant 消息
#[cfg(test)]
pub(super) fn convert_assistant_message(
    msg: &AnthropicMessage,
    tool_name_map: &mut HashMap<String, String>,
    options: ConverterOptions,
) -> Result<HistoryAssistantMessage, ConversionError> {
    let mut known_tool_names = tool_name_map.keys().cloned().collect::<HashSet<_>>();
    known_tool_names.extend(tool_name_map.values().cloned());
    let mut warnings = ProxyWarnings::default();
    convert_assistant_message_with_known_tools(
        msg,
        tool_name_map,
        &known_tool_names,
        &mut warnings,
        options,
    )
}

fn convert_assistant_message_with_known_tools(
    msg: &AnthropicMessage,
    tool_name_map: &mut HashMap<String, String>,
    known_tool_names: &HashSet<String>,
    warnings: &mut ProxyWarnings,
    options: ConverterOptions,
) -> Result<HistoryAssistantMessage, ConversionError> {
    let mut thinking_content = String::new();
    let mut native_reasoning_content = None;
    let mut text_content = String::new();
    let mut tool_uses = Vec::new();
    let mut sanitizer = (options.compat_profile != CompatProfile::AnthropicStrict)
        .then(|| ToolTranscriptSanitizer::new(known_tool_names.iter().cloned()));

    match &msg.content {
        serde_json::Value::String(s) => {
            if let Some(sanitizer) = sanitizer.as_mut() {
                text_content.push_str(&sanitizer.push(s));
            } else {
                text_content.push_str(s);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    match block.block_type.as_str() {
                        "thinking" => {
                            let thinking = block.thinking.ok_or_else(|| {
                                ConversionError::UnsupportedContent(
                                    "assistant thinking block is missing thinking text".to_string(),
                                )
                            })?;
                            if let Some(signature) =
                                block.signature.filter(|signature| !signature.is_empty())
                            {
                                set_native_reasoning_content(
                                    &mut native_reasoning_content,
                                    ReasoningContent::reasoning_text(thinking, signature),
                                )?;
                            } else {
                                thinking_content.push_str(&thinking);
                            }
                        }
                        "redacted_thinking" => {
                            let data = block.data.ok_or_else(|| {
                                ConversionError::UnsupportedContent(
                                    "assistant redacted_thinking block is missing data".to_string(),
                                )
                            })?;
                            validate_redacted_thinking_data(&data).map_err(|message| {
                                ConversionError::UnsupportedContent(message.to_string())
                            })?;
                            set_native_reasoning_content(
                                &mut native_reasoning_content,
                                ReasoningContent::redacted_content(data),
                            )?;
                        }
                        "text" => {
                            if let Some(text) = block.text {
                                if let Some(sanitizer) = sanitizer.as_mut() {
                                    text_content.push_str(&sanitizer.push(&text));
                                } else {
                                    text_content.push_str(&text);
                                }
                            }
                        }
                        "tool_use" => {
                            if let Some(sanitizer) = sanitizer.as_mut() {
                                text_content.push_str(&sanitizer.structured_tool_boundary());
                            }
                            if let (Some(id), Some(name)) = (
                                block.id.as_deref().and_then(sanitize_tool_use_id),
                                block
                                    .name
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|name| !name.is_empty()),
                            ) {
                                let input = normalize_tool_use_input(
                                    block.input.unwrap_or(serde_json::json!({})),
                                );
                                let mapped_name =
                                    map_tool_name(name, tool_name_map, options.clone());
                                tool_uses
                                    .push(ToolUseEntry::new(id, mapped_name).with_input(input));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    if let Some(mut sanitizer) = sanitizer {
        text_content.push_str(&sanitizer.finish());
        if sanitizer.suppressed_blocks() > 0 {
            warnings.sanitized_assistant_history_leaks = warnings
                .sanitized_assistant_history_leaks
                .saturating_add(sanitizer.suppressed_blocks());
            warnings.sanitized_assistant_history_leak_chars = warnings
                .sanitized_assistant_history_leak_chars
                .saturating_add(sanitizer.suppressed_chars().min(u32::MAX as usize) as u32);
            tracing::warn!(
                suppressed_blocks = sanitizer.suppressed_blocks(),
                suppressed_chars = sanitizer.suppressed_chars(),
                suppressed_kinds = ?sanitizer.matched_kinds(),
                "sanitized internal tool transcript from assistant history"
            );
        }
    }

    if native_reasoning_content.is_some() && !thinking_content.is_empty() {
        return Err(ConversionError::UnsupportedContent(
            "assistant history mixes native signed/redacted reasoning with unsigned thinking and cannot be represented losslessly"
                .to_string(),
        ));
    }

    // 组合 unsigned thinking 和 text 内容
    // 格式: <thinking>思考内容</thinking>\n\ntext内容
    // 注意: Kiro API 要求 content 字段不能为空，当只有 tool_use 时需要占位符
    let final_content = if !thinking_content.is_empty() {
        if !text_content.is_empty() {
            format!(
                "<thinking>{}</thinking>\n\n{}",
                thinking_content, text_content
            )
        } else {
            format!("<thinking>{}</thinking>", thinking_content)
        }
    } else if text_content.is_empty() {
        " ".to_string()
    } else {
        text_content
    };

    let mut assistant = AssistantMessage::new(final_content);
    if !tool_uses.is_empty() {
        assistant = assistant.with_tool_uses(tool_uses);
    }
    if let Some(reasoning_content) = native_reasoning_content {
        assistant = assistant.with_reasoning_content(reasoning_content);
    }

    Ok(HistoryAssistantMessage {
        assistant_response_message: assistant,
    })
}

fn set_native_reasoning_content(
    current: &mut Option<ReasoningContent>,
    next: ReasoningContent,
) -> Result<(), ConversionError> {
    if current.is_some() {
        return Err(ConversionError::UnsupportedContent(
            "assistant history contains multiple or mixed native reasoning blocks; Kiro accepts one reasoningContent union value per assistant message"
                .to_string(),
        ));
    }
    *current = Some(next);
    Ok(())
}

/// 合并多个连续的 assistant 消息为一条
/// 用于处理网络不稳定时产生的连续 assistant 消息（Issue #79）
#[cfg(test)]
pub(super) fn merge_assistant_messages(
    messages: &[&AnthropicMessage],
    tool_name_map: &mut HashMap<String, String>,
    options: ConverterOptions,
) -> Result<HistoryAssistantMessage, ConversionError> {
    let mut known_tool_names = tool_name_map.keys().cloned().collect::<HashSet<_>>();
    known_tool_names.extend(tool_name_map.values().cloned());
    let mut warnings = ProxyWarnings::default();
    merge_assistant_messages_with_known_tools(
        messages,
        tool_name_map,
        &known_tool_names,
        &mut warnings,
        options,
    )
}

fn merge_assistant_messages_with_known_tools(
    messages: &[&AnthropicMessage],
    tool_name_map: &mut HashMap<String, String>,
    known_tool_names: &HashSet<String>,
    warnings: &mut ProxyWarnings,
    options: ConverterOptions,
) -> Result<HistoryAssistantMessage, ConversionError> {
    assert!(!messages.is_empty());
    let mut all_tool_uses: Vec<ToolUseEntry> = Vec::new();
    let mut native_reasoning_content = None;
    let mut content_parts: Vec<(String, Option<usize>)> = Vec::new();
    let mut sequence_known_tool_names = known_tool_names.clone();
    for message in messages {
        if let Some(blocks) = message.content.as_array() {
            sequence_known_tool_names.extend(blocks.iter().filter_map(|block| {
                (block.get("type").and_then(|value| value.as_str()) == Some("tool_use"))
                    .then(|| block.get("name").and_then(|value| value.as_str()))
                    .flatten()
                    .map(str::to_string)
            }));
        }
    }
    for msg in messages {
        let converted = convert_assistant_message_with_known_tools(
            msg,
            tool_name_map,
            &sequence_known_tool_names,
            warnings,
            options.clone(),
        )?;
        let am = converted.assistant_response_message;
        if !am.content.trim().is_empty() {
            let thinking_prefix_len = generated_thinking_prefix_len(msg, &am.content);
            content_parts.push((am.content, thinking_prefix_len));
        }
        if let Some(tus) = am.tool_uses {
            all_tool_uses.extend(tus);
        }
        if let Some(reasoning_content) = am.reasoning_content {
            set_native_reasoning_content(&mut native_reasoning_content, reasoning_content).map_err(
                |_| {
                    ConversionError::UnsupportedContent(
                        "consecutive assistant messages contain multiple native reasoning blocks and cannot be merged losslessly"
                            .to_string(),
                    )
                },
            )?;
        }
    }

    // A Kiro assistant history item flattens every Anthropic visible text block into one string and
    // joins consecutive assistant messages with two newlines. Scan that reconstructed visible text
    // as a second line of defence: source block boundaries can otherwise hide a scaffold that only
    // becomes complete after flattening or joining. Generated `<thinking>` prefixes are preserved as
    // trusted boundaries because altering their body independently would leave malformed history.
    let content = if options.compat_profile == CompatProfile::AnthropicStrict {
        content_parts
            .into_iter()
            .map(|(content, _)| content)
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        let mut final_known_tool_names = sequence_known_tool_names;
        final_known_tool_names.extend(tool_name_map.keys().cloned());
        final_known_tool_names.extend(tool_name_map.values().cloned());
        let final_known_tool_names = final_known_tool_names.into_iter().collect::<Vec<_>>();
        let mut sanitizer = ToolTranscriptSanitizer::new(final_known_tool_names.iter().cloned());
        let mut sanitized = String::new();
        let mut suppressed_blocks = 0u32;
        let mut suppressed_chars = 0usize;
        let mut suppressed_kinds = HashSet::new();
        for (index, (part, thinking_prefix_len)) in content_parts.into_iter().enumerate() {
            if let Some(prefix_len) = thinking_prefix_len {
                sanitized.push_str(&sanitizer.finish());
                suppressed_blocks = suppressed_blocks.saturating_add(sanitizer.suppressed_blocks());
                suppressed_chars = suppressed_chars.saturating_add(sanitizer.suppressed_chars());
                suppressed_kinds.extend(sanitizer.matched_kinds().into_iter().map(str::to_string));
                sanitizer = ToolTranscriptSanitizer::new(final_known_tool_names.iter().cloned());

                if index > 0 {
                    sanitized.push_str("\n\n");
                }
                sanitized.push_str(&part[..prefix_len]);
                sanitized.push_str(&sanitizer.push(&part[prefix_len..]));
            } else {
                if index > 0 {
                    sanitized.push_str(&sanitizer.push("\n\n"));
                }
                sanitized.push_str(&sanitizer.push(&part));
            }
        }
        sanitized.push_str(&sanitizer.finish());
        suppressed_blocks = suppressed_blocks.saturating_add(sanitizer.suppressed_blocks());
        suppressed_chars = suppressed_chars.saturating_add(sanitizer.suppressed_chars());
        suppressed_kinds.extend(sanitizer.matched_kinds().into_iter().map(str::to_string));
        if suppressed_blocks > 0 {
            warnings.sanitized_assistant_history_leaks = warnings
                .sanitized_assistant_history_leaks
                .saturating_add(suppressed_blocks);
            warnings.sanitized_assistant_history_leak_chars = warnings
                .sanitized_assistant_history_leak_chars
                .saturating_add(suppressed_chars.min(u32::MAX as usize) as u32);
            tracing::warn!(
                suppressed_blocks,
                suppressed_chars,
                suppressed_kinds = ?suppressed_kinds,
                "sanitized internal tool transcript reconstructed while merging assistant history"
            );
        }
        sanitized
    };
    let content = if content.trim().is_empty() {
        " ".to_string()
    } else {
        content
    };

    let mut assistant = AssistantMessage::new(content);
    if !all_tool_uses.is_empty() {
        assistant = assistant.with_tool_uses(all_tool_uses);
    }
    if let Some(reasoning_content) = native_reasoning_content {
        assistant = assistant.with_reasoning_content(reasoning_content);
    }
    Ok(HistoryAssistantMessage {
        assistant_response_message: assistant,
    })
}

fn generated_thinking_prefix_len(msg: &AnthropicMessage, content: &str) -> Option<usize> {
    let thinking = msg
        .content
        .as_array()?
        .iter()
        .filter_map(|item| serde_json::from_value::<ContentBlock>(item.clone()).ok())
        .filter(|block| block.block_type == "thinking")
        .filter(|block| {
            !block
                .signature
                .as_deref()
                .is_some_and(|signature| !signature.is_empty())
        })
        .filter_map(|block| block.thinking)
        .collect::<String>();
    if thinking.is_empty() {
        return None;
    }

    let prefix = format!("<thinking>{thinking}</thinking>");
    if content == prefix {
        Some(prefix.len())
    } else {
        let prefix = format!("{prefix}\n\n");
        content.starts_with(&prefix).then_some(prefix.len())
    }
}
