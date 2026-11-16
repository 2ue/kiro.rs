use crate::model::config::ImageProcessingConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyProcessingProfile {
    SharedParsedAnthropic,
    ExternalRaw,
}

impl BodyProcessingProfile {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SharedParsedAnthropic => "shared_parsed_anthropic",
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
