//! 不建 DOM，只读出 Kiro 请求体里的少数几个字段。
//!
//! 请求体常有几 MB。用 `serde_json::Value` 整包解析时，每个字符串、数组、对象都要单独分配，
//! 而且每次发送、每次换号重试都要重来一遍。这里只取调度和端点转换要用的字段，其余部分边读边丢：
//! 没有转义的字符串直接借用请求体，不建树，也不分配。
//!
//! 能否解析成功和 `serde_json::from_str::<Value>` 完全一致：用的是同一个解析器，跳过时也走
//! `deserialize_any`，所以嵌套深度同样限制在 128 层。取值规则也和 `Value` 的读法一致：同名键
//! 以最后一个为准，类型不对的字段视为缺失，键名中的转义（如 `"conversationId"`）先解码再比较。

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// 请求体根对象上，调度和端点转换关心的几个字段。
#[derive(Debug, Default)]
pub(crate) struct KiroRequestHead<'a> {
    /// 根是不是 JSON 对象。
    pub(crate) is_object: bool,
    /// `conversationState.conversationId`；不是字符串时为 `None`。
    pub(crate) conversation_id: Option<Cow<'a, str>>,
    /// `conversationState.agentContinuationId`；不是字符串时为 `None`。
    pub(crate) agent_continuation_id: Option<Cow<'a, str>>,
    /// `conversationState.currentMessage.userInputMessage.modelId`。
    pub(crate) model_id: Option<Cow<'a, str>>,
    /// 根上的 `profileArn`。外层表示这个键是否存在，内层是字符串值（不是字符串时为 `None`）。
    pub(crate) profile_arn: Option<Option<Cow<'a, str>>>,
    /// `additionalModelRequestFields` 是否需要按
    /// `endpoint::normalize_output_config_thinking_compatibility_json` 的规则去掉 `thinking`。
    pub(crate) reasoning_fields_need_normalization: bool,
}

impl<'a> KiroRequestHead<'a> {
    /// 不是合法 JSON 时返回 `None`。
    pub(crate) fn parse(body: &'a str) -> Option<Self> {
        serde_json::from_str::<Root<'a>>(body)
            .ok()
            .map(|root| root.0)
    }
}

/// 只接受 JSON 对象的访问器：其他类型一律当作默认值，数组逐项跳过。
macro_rules! non_object_is_default {
    ($de:lifetime) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_seq<A: SeqAccess<$de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq)?;
            Ok(Default::default())
        }
    };
}

/// 为只关心对象成员的类型生成 `Deserialize`：`$body` 逐个处理键值对。
macro_rules! object_visitor {
    ($ty:ident<$de:lifetime>, $expecting:literal, |$value:ident, $map:ident, $key:ident| $body:block) => {
        impl<$de> Deserialize<$de> for $ty<$de> {
            fn deserialize<D: Deserializer<$de>>(deserializer: D) -> Result<Self, D::Error> {
                struct ObjectVisitor;
                impl<$de> Visitor<$de> for ObjectVisitor {
                    type Value = $ty<$de>;
                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str($expecting)
                    }
                    non_object_is_default!($de);
                    fn visit_map<A: MapAccess<$de>>(
                        self,
                        mut $map: A,
                    ) -> Result<Self::Value, A::Error> {
                        let mut $value = $ty::default();
                        $value.mark_object();
                        while let Some(Key($key)) = $map.next_key::<Key<$de>>()? {
                            $body
                        }
                        Ok($value)
                    }
                }
                deserializer.deserialize_any(ObjectVisitor)
            }
        }
    };
}

/// 解析到 JSON 对象（哪怕是空对象 `{}`）时调用；只有需要区分“是对象”的类型才覆盖。
trait ObjectMarker {
    fn mark_object(&mut self) {}
}

fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<Skip>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<Skip, Skip>()?.is_some() {}
    Ok(())
}

/// 读过一个任意值并丢弃，只记下它是不是 `null`。
#[derive(Default)]
struct Skip {
    non_null: bool,
}

impl<'de> Deserialize<'de> for Skip {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SkipVisitor;
        impl<'de> Visitor<'de> for SkipVisitor {
            type Value = Skip;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Skip, E> {
                Ok(Skip { non_null: true })
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Skip, E> {
                Ok(Skip { non_null: true })
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Skip, E> {
                Ok(Skip { non_null: true })
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Skip, E> {
                Ok(Skip { non_null: true })
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Skip, E> {
                Ok(Skip { non_null: true })
            }
            fn visit_unit<E: de::Error>(self) -> Result<Skip, E> {
                Ok(Skip { non_null: false })
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Skip, A::Error> {
                drain_seq(seq)?;
                Ok(Skip { non_null: true })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Skip, A::Error> {
                drain_map(map)?;
                Ok(Skip { non_null: true })
            }
        }
        deserializer.deserialize_any(SkipVisitor)
    }
}

/// 对象键。没有转义时直接借用请求体。
struct Key<'a>(Cow<'a, str>);

impl<'de> Deserialize<'de> for Key<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = Key<'de>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object key")
            }
            fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Borrowed(value)))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Owned(value.to_owned())))
            }
        }
        deserializer.deserialize_str(KeyVisitor)
    }
}

/// 字符串值；其他类型都当作 `None`，相当于 `Value::as_str`。
#[derive(Default)]
struct Str<'a>(Option<Cow<'a, str>>);

impl<'de> Deserialize<'de> for Str<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrVisitor;
        impl<'de> Visitor<'de> for StrVisitor {
            type Value = Str<'de>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Str<'de>, E> {
                Ok(Str(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Str<'de>, E> {
                Ok(Str(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Str<'de>, E> {
                Ok(Str(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Str<'de>, E> {
                Ok(Str(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Str<'de>, E> {
                Ok(Str(None))
            }
            fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Str<'de>, E> {
                Ok(Str(Some(Cow::Borrowed(value))))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Str<'de>, E> {
                Ok(Str(Some(Cow::Owned(value.to_owned()))))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Str<'de>, A::Error> {
                drain_seq(seq)?;
                Ok(Str(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Str<'de>, A::Error> {
                drain_map(map)?;
                Ok(Str(None))
            }
        }
        deserializer.deserialize_any(StrVisitor)
    }
}

#[derive(Default)]
struct Root<'a>(KiroRequestHead<'a>);

impl ObjectMarker for Root<'_> {
    fn mark_object(&mut self) {
        self.0.is_object = true;
    }
}

object_visitor!(Root<'de>, "a Kiro request object", |root, map, key| {
    match key.as_ref() {
        "conversationState" => {
            let state = map.next_value::<ConversationState<'de>>()?;
            root.0.conversation_id = state.conversation_id;
            root.0.agent_continuation_id = state.agent_continuation_id;
            root.0.model_id = state.model_id;
        }
        "profileArn" => root.0.profile_arn = Some(map.next_value::<Str<'de>>()?.0),
        "additionalModelRequestFields" => {
            root.0.reasoning_fields_need_normalization = map
                .next_value::<AdditionalModelRequestFields<'de>>()?
                .needs_normalization();
        }
        _ => {
            map.next_value::<Skip>()?;
        }
    }
});

#[derive(Default)]
struct ConversationState<'a> {
    conversation_id: Option<Cow<'a, str>>,
    agent_continuation_id: Option<Cow<'a, str>>,
    model_id: Option<Cow<'a, str>>,
}

impl ObjectMarker for ConversationState<'_> {}

object_visitor!(
    ConversationState<'de>,
    "a conversationState object",
    |state, map, key| {
        match key.as_ref() {
            "conversationId" => state.conversation_id = map.next_value::<Str<'de>>()?.0,
            "agentContinuationId" => state.agent_continuation_id = map.next_value::<Str<'de>>()?.0,
            "currentMessage" => state.model_id = map.next_value::<CurrentMessage<'de>>()?.0,
            _ => {
                map.next_value::<Skip>()?;
            }
        }
    }
);

#[derive(Default)]
struct CurrentMessage<'a>(Option<Cow<'a, str>>);

impl ObjectMarker for CurrentMessage<'_> {}

object_visitor!(
    CurrentMessage<'de>,
    "a currentMessage object",
    |message, map, key| {
        if key == "userInputMessage" {
            message.0 = map.next_value::<UserInputMessage<'de>>()?.0;
        } else {
            map.next_value::<Skip>()?;
        }
    }
);

#[derive(Default)]
struct UserInputMessage<'a>(Option<Cow<'a, str>>);

impl ObjectMarker for UserInputMessage<'_> {}

object_visitor!(
    UserInputMessage<'de>,
    "a userInputMessage object",
    |message, map, key| {
        if key == "modelId" {
            message.0 = map.next_value::<Str<'de>>()?.0;
        } else {
            map.next_value::<Skip>()?;
        }
    }
);

#[derive(Default)]
struct AdditionalModelRequestFields<'a> {
    output_config_non_null: bool,
    /// `thinking` 存在时，它是否和 `output_config` 冲突。
    thinking_incompatible: Option<bool>,
    _body: std::marker::PhantomData<&'a str>,
}

impl AdditionalModelRequestFields<'_> {
    /// 与 `normalize_output_config_thinking_compatibility_json` 的判断一致：`output_config`
    /// 非空，且 `thinking` 存在但不是 `{"type":"adaptive"}`。
    fn needs_normalization(&self) -> bool {
        self.output_config_non_null && self.thinking_incompatible == Some(true)
    }
}

impl ObjectMarker for AdditionalModelRequestFields<'_> {}

object_visitor!(
    AdditionalModelRequestFields<'de>,
    "an additionalModelRequestFields object",
    |fields, map, key| {
        match key.as_ref() {
            "output_config" => fields.output_config_non_null = map.next_value::<Skip>()?.non_null,
            "thinking" => {
                fields.thinking_incompatible =
                    Some(map.next_value::<Thinking<'de>>()?.incompatible());
            }
            _ => {
                map.next_value::<Skip>()?;
            }
        }
    }
);

/// `thinking` 字段。不是对象（包括 `null`）时视为不兼容，和 `Value` 版本的判断一致。
#[derive(Default)]
struct Thinking<'a> {
    is_object: bool,
    thinking_type: Option<Cow<'a, str>>,
}

impl Thinking<'_> {
    fn incompatible(&self) -> bool {
        !self.is_object || self.thinking_type.as_deref() != Some("adaptive")
    }
}

impl ObjectMarker for Thinking<'_> {
    fn mark_object(&mut self) {
        self.is_object = true;
    }
}

object_visitor!(Thinking<'de>, "a thinking object", |thinking, map, key| {
    if key == "type" {
        thinking.thinking_type = map.next_value::<Str<'de>>()?.0;
    } else {
        map.next_value::<Skip>()?;
    }
});

#[cfg(test)]
mod tests {
    use super::KiroRequestHead;
    use crate::http_client::allocation_probe;
    use serde_json::Value;

    /// (is_object, conversationId, agentContinuationId, modelId, profileArn, 需要改写推理字段)
    type Fields = (
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Option<String>>,
        bool,
    );

    /// 用 `Value` 按原来的写法读同样的字段，作为对照。
    fn expected(body: &str) -> Option<Fields> {
        let mut json: Value = serde_json::from_str(body).ok()?;
        let state = json.get("conversationState");
        let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
        let conversation_id = text(state.and_then(|state| state.get("conversationId")));
        let continuation_id = text(state.and_then(|state| state.get("agentContinuationId")));
        let model_id = text(
            state
                .and_then(|state| state.get("currentMessage"))
                .and_then(|message| message.get("userInputMessage"))
                .and_then(|message| message.get("modelId")),
        );
        let profile_arn = json
            .as_object()
            .and_then(|root| root.get("profileArn"))
            .map(|value| value.as_str().map(str::to_string));
        let is_object = json.is_object();
        let needs_normalization =
            crate::kiro::endpoint::normalize_output_config_thinking_compatibility_json(&mut json);
        Some((
            is_object,
            conversation_id,
            continuation_id,
            model_id,
            profile_arn,
            needs_normalization,
        ))
    }

    fn actual(body: &str) -> Option<Fields> {
        let head = KiroRequestHead::parse(body)?;
        let owned =
            |value: Option<std::borrow::Cow<'_, str>>| value.map(|value| value.into_owned());
        Some((
            head.is_object,
            owned(head.conversation_id),
            owned(head.agent_continuation_id),
            owned(head.model_id),
            head.profile_arn.map(owned),
            head.reasoning_fields_need_normalization,
        ))
    }

    #[test]
    fn reads_the_same_fields_as_value_for_five_rounds() {
        let deep = format!("{{\"a\":{}0{}}}", "[".repeat(200), "]".repeat(200));
        let cases = [
            r#"{"conversationState":{"conversationId":"c1","agentContinuationId":"a1","currentMessage":{"userInputMessage":{"modelId":"m1","content":"x"}}},"profileArn":"arn"}"#,
            r#"{}"#,
            r#"  { }  "#,
            r#"[]"#,
            r#""text""#,
            r#"null"#,
            r#"42"#,
            r#"not json"#,
            r#"{"a":1,}"#,
            r#"{"a":1} trailing"#,
            // 同名键以最后一个为准，包括被覆盖成别的类型。
            r#"{"conversationState":{"conversationId":"old","conversationId":"new"},"conversationState":{"conversationId":"last"}}"#,
            r#"{"conversationState":{"conversationId":"c1","conversationId":7}}"#,
            r#"{"profileArn":"a","profileArn":null}"#,
            r#"{"profileArn":["x"]}"#,
            // 键名转义要先解码。
            r#"{"conversationState":{"conversationId":"esc\"aped","model":"x"}}"#,
            r#"{"conversationState":[{"conversationId":"in-array"}]}"#,
            r#"{"conversationState":{"currentMessage":{"userInputMessage":{"modelId":{"nested":"m"}}}}}"#,
            r#"{"conversationState":{"currentMessage":{"userInputMessage":"str"}}}"#,
            r#"{"history":[{"conversationId":"not-root"}],"conversationState":{"history":[{"conversationId":"x"}]}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{"effort":"low"},"thinking":{"type":"disabled"}}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{"effort":"low"},"thinking":{"type":"adaptive"}}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{"effort":"low"},"thinking":{}}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{"effort":"low"},"thinking":null}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{"effort":"low"},"thinking":"enabled"}}"#,
            r#"{"additionalModelRequestFields":{"output_config":null,"thinking":{"type":"disabled"}}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{"effort":"low"}}}"#,
            r#"{"additionalModelRequestFields":{"thinking":{"type":"disabled"}}}"#,
            r#"{"additionalModelRequestFields":{"output_config":0,"thinking":{"type":"adaptive","type":"disabled"}}}"#,
            r#"{"additionalModelRequestFields":{"output_config":{},"thinking":{"type":"disabled"}},"additionalModelRequestFields":{}}"#,
            r#"{"additionalModelRequestFields":"x"}"#,
            r#"{"output_config":{"effort":"low"},"thinking":{"type":"disabled"}}"#,
            r#"{"z":1e400}"#,
            r#"{"z":18446744073709551616,"conversationState":{"conversationId":"big-number"}}"#,
            r#"{"s":"\ud800"}"#,
            deep.as_str(),
        ];
        let too_deep = format!("{{\"a\":{}0{}}}", "[".repeat(300), "]".repeat(300));

        for round in 0..5 {
            for body in cases {
                assert_eq!(actual(body), expected(body), "round {round}: {body}");
            }
            assert!(serde_json::from_str::<Value>(&too_deep).is_err());
            assert!(KiroRequestHead::parse(&too_deep).is_none(), "round {round}");
        }
    }

    #[test]
    fn borrows_unescaped_strings_and_skips_large_history_without_allocating() {
        let mut body =
            String::from(r#"{"conversationState":{"conversationId":"conv-1","history":["#);
        for index in 0..20_000 {
            if index > 0 {
                body.push(',');
            }
            body.push_str(
                r#"{"userInputMessage":{"content":"some history text","images":[1,2,3]}}"#,
            );
        }
        body.push_str(
            r#"],"currentMessage":{"userInputMessage":{"modelId":"claude-sonnet-4.5"}}},"profileArn":"arn:x"}"#,
        );

        // 计数器是进程级的，并行测试的分配也会算进来，所以和整包解析成 `Value` 对比，不用绝对值。
        for round in 0..5 {
            let (head, stats) = allocation_probe::measure(|| KiroRequestHead::parse(&body));
            let head = head.expect("valid body");
            assert!(matches!(
                head.conversation_id,
                Some(std::borrow::Cow::Borrowed("conv-1"))
            ));
            assert!(matches!(
                head.model_id,
                Some(std::borrow::Cow::Borrowed("claude-sonnet-4.5"))
            ));
            let (value, value_stats) =
                allocation_probe::measure(|| serde_json::from_str::<Value>(&body));
            drop(value);
            assert!(
                stats.allocation_ops.saturating_mul(20) < value_stats.allocation_ops,
                "round {round}: head parse allocated {} times, Value parse {} times",
                stats.allocation_ops,
                value_stats.allocation_ops
            );
        }
    }
}
