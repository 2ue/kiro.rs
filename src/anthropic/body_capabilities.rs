use crate::model::config::ImageProcessingConfig;

use super::payload_guard::PayloadGuardConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyProcessingProfile {
    SharedParsedAnthropic,
    ExternalNormalized,
    ExternalRaw,
}

impl BodyProcessingProfile {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SharedParsedAnthropic => "shared_parsed_anthropic",
            Self::ExternalNormalized => "external_normalized",
            Self::ExternalRaw => "external_raw",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyStageState {
    Disabled,
    Enabled,
}

impl BodyStageState {
    pub(crate) fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

impl From<bool> for BodyStageState {
    fn from(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThinkingStagePlan {
    pub(crate) model_name_override: BodyStageState,
    pub(crate) trigger_mode: BodyStageState,
    pub(crate) trace: BodyStageState,
}

impl ThinkingStagePlan {
    pub(crate) fn compatible_default() -> Self {
        Self {
            model_name_override: BodyStageState::Enabled,
            trigger_mode: BodyStageState::Enabled,
            trace: BodyStageState::Enabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MultimodalStageKind {
    Disabled,
    Configured(ImageProcessingConfig),
}

impl MultimodalStageKind {
    pub(crate) fn is_enabled(self) -> bool {
        matches!(self, Self::Configured(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedAnthropicBodyPlan {
    pub(crate) profile: BodyProcessingProfile,
    pub(crate) thinking: ThinkingStagePlan,
    pub(crate) multimodal: MultimodalStageKind,
}

impl ParsedAnthropicBodyPlan {
    pub(crate) fn shared_compatible(image_processing: ImageProcessingConfig) -> Self {
        Self {
            profile: BodyProcessingProfile::SharedParsedAnthropic,
            thinking: ThinkingStagePlan::compatible_default(),
            multimodal: MultimodalStageKind::Configured(image_processing.normalized()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn raw_probe_only() -> Self {
        Self {
            profile: BodyProcessingProfile::ExternalRaw,
            thinking: ThinkingStagePlan {
                model_name_override: BodyStageState::Disabled,
                trigger_mode: BodyStageState::Disabled,
                trace: BodyStageState::Disabled,
            },
            multimodal: MultimodalStageKind::Disabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PayloadGuardStagePlan {
    pub(crate) state: BodyStageState,
    pub(crate) config: PayloadGuardConfig,
}

impl PayloadGuardStagePlan {
    pub(crate) fn from_config(config: PayloadGuardConfig) -> Self {
        Self {
            state: BodyStageState::from(config.enabled),
            config,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawModelStagePlan {
    None,
    ProbeOnly,
    RewriteTopLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalBodyBytesPlan {
    RawPassthrough {
        model: RawModelStagePlan,
    },
    Normalized {
        payload_guard: PayloadGuardStagePlan,
        model: BodyStageState,
        thinking_normalization: BodyStageState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExternalBodyPlan {
    pub(crate) profile: BodyProcessingProfile,
    pub(crate) bytes: ExternalBodyBytesPlan,
    pub(crate) usage_projection: BodyStageState,
}

impl ExternalBodyPlan {
    pub(crate) fn raw(model: RawModelStagePlan) -> Self {
        Self {
            profile: BodyProcessingProfile::ExternalRaw,
            bytes: ExternalBodyBytesPlan::RawPassthrough { model },
            usage_projection: BodyStageState::Enabled,
        }
    }

    pub(crate) fn normalized(payload_guard_config: PayloadGuardConfig) -> Self {
        Self {
            profile: BodyProcessingProfile::ExternalNormalized,
            bytes: ExternalBodyBytesPlan::Normalized {
                payload_guard: PayloadGuardStagePlan::from_config(payload_guard_config),
                model: BodyStageState::Enabled,
                thinking_normalization: BodyStageState::Enabled,
            },
            usage_projection: BodyStageState::Enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::config::PayloadShapingConfig;

    use super::*;

    fn payload_guard_config(enabled: bool) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled,
            max_bytes: 1234,

            trim_history: true,
            shaping: PayloadShapingConfig::default(),
        }
    }

    #[test]
    fn parsed_shared_compatible_enables_existing_preprocessing_stages() {
        let plan = ParsedAnthropicBodyPlan::shared_compatible(ImageProcessingConfig::default());

        assert_eq!(plan.profile, BodyProcessingProfile::SharedParsedAnthropic);
        assert!(plan.thinking.model_name_override.is_enabled());
        assert!(plan.thinking.trigger_mode.is_enabled());
        assert!(plan.thinking.trace.is_enabled());
        assert!(plan.multimodal.is_enabled());
    }

    #[test]
    fn raw_probe_only_disables_parsed_body_work() {
        let plan = ParsedAnthropicBodyPlan::raw_probe_only();

        assert_eq!(plan.profile, BodyProcessingProfile::ExternalRaw);
        assert!(!plan.thinking.model_name_override.is_enabled());
        assert!(!plan.thinking.trigger_mode.is_enabled());
        assert!(!plan.thinking.trace.is_enabled());
        assert!(!plan.multimodal.is_enabled());
    }

    #[test]
    fn external_raw_keeps_usage_separate_from_body_processing() {
        let plan = ExternalBodyPlan::raw(RawModelStagePlan::ProbeOnly);

        assert_eq!(plan.profile, BodyProcessingProfile::ExternalRaw);
        assert!(plan.usage_projection.is_enabled());
        assert_eq!(
            plan.bytes,
            ExternalBodyBytesPlan::RawPassthrough {
                model: RawModelStagePlan::ProbeOnly
            }
        );
    }

    #[test]
    fn external_normalized_mounts_payload_guard_and_model_stages() {
        let plan = ExternalBodyPlan::normalized(payload_guard_config(false));

        assert_eq!(plan.profile, BodyProcessingProfile::ExternalNormalized);
        assert!(plan.usage_projection.is_enabled());
        let ExternalBodyBytesPlan::Normalized {
            payload_guard,
            model,
            thinking_normalization,
        } = plan.bytes
        else {
            panic!("expected normalized plan");
        };
        assert!(!payload_guard.state.is_enabled());
        assert!(model.is_enabled());
        assert!(thinking_normalization.is_enabled());
    }
}
