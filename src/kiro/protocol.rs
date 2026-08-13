//! Shared Kiro upstream protocol helpers.

use serde_json::Value;

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::{Config, KiroAgentModeStrategy};

pub const KIRO_BUILDER_ID_PLACEHOLDER_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
pub const KIRO_SOCIAL_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";

const ENTERPRISE_FALLBACK_PROFILE_ID: &str = "VNECVYCYYAWN";
const ENTERPRISE_FALLBACK_ACCOUNT_ID: &str = "610548660232";

fn compact_protocol_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

pub fn is_placeholder_profile_arn(arn: &str) -> bool {
    arn == KIRO_BUILDER_ID_PLACEHOLDER_ARN
}

pub fn is_enterprise_fallback_profile_arn(arn: &str) -> bool {
    arn == enterprise_fallback_profile_arn("us-east-1")
        || arn == enterprise_fallback_profile_arn("eu-central-1")
}

pub fn is_real_profile_arn(arn: &str) -> bool {
    !is_placeholder_profile_arn(arn) && !is_enterprise_fallback_profile_arn(arn)
}

pub fn enterprise_fallback_profile_arn(region: &str) -> String {
    let region = if region.starts_with("eu-") {
        "eu-central-1"
    } else {
        "us-east-1"
    };
    format!(
        "arn:aws:codewhisperer:{}:{}:profile/{}",
        region, ENTERPRISE_FALLBACK_ACCOUNT_ID, ENTERPRISE_FALLBACK_PROFILE_ID
    )
}

pub fn is_external_idp_credentials(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }

    credentials
        .auth_method
        .as_deref()
        .is_some_and(is_external_idp_auth_method)
        || credentials
            .provider
            .as_deref()
            .is_some_and(is_external_idp_provider)
}

pub fn is_external_idp_auth_method(value: &str) -> bool {
    matches!(
        compact_protocol_value(value).as_str(),
        "externalidp" | "enterprise" | "iamsso" | "awsidc" | "internal"
    )
}

pub fn is_external_idp_provider(value: &str) -> bool {
    matches!(
        compact_protocol_value(value).as_str(),
        "enterprise" | "externalidp" | "iamsso" | "awsidc" | "internal"
    )
}

pub fn is_social_credentials(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }

    credentials
        .auth_method
        .as_deref()
        .is_some_and(|value| compact_protocol_value(value) == "social")
        || credentials.provider.as_deref().is_some_and(|value| {
            matches!(compact_protocol_value(value).as_str(), "github" | "google")
        })
}

/// Resolve a profile ARN that is safe to send as an upstream identity selector.
///
/// Header/query endpoints such as MCP, ListAvailableModels, and usage APIs must not
/// receive BuilderId placeholders or Enterprise fallback ARNs. Those values are not
/// caller-owned real profiles and can make otherwise valid accounts fail with 400/403.
pub fn resolve_profile_arn(credentials: &KiroCredentials, _config: &Config) -> Option<String> {
    if credentials.is_api_key_credential() {
        return None;
    }

    if let Some(profile_arn) = credentials
        .profile_arn
        .as_deref()
        .map(str::trim)
        .filter(|arn| !arn.is_empty() && is_real_profile_arn(arn))
    {
        return Some(profile_arn.to_string());
    }

    if is_social_credentials(credentials) {
        return Some(KIRO_SOCIAL_PROFILE_ARN.to_string());
    }

    None
}

/// Resolve the profile ARN for streaming assistant request bodies.
///
/// Streaming calls are stricter than header/query APIs: BuilderId/free OAuth flows
/// still need a body-level `profileArn`, while API-key credentials have no profile
/// concept. Enterprise/IdC should self-heal to a real ARN first; the region-aware
/// fallback here is request-body-only and must not be persisted as a real profile.
pub fn resolve_streaming_profile_arn(
    credentials: &KiroCredentials,
    config: &Config,
) -> Option<String> {
    if credentials.is_api_key_credential() {
        return None;
    }

    if let Some(profile_arn) = credentials
        .profile_arn
        .as_deref()
        .map(str::trim)
        .filter(|arn| !arn.is_empty())
    {
        return Some(profile_arn.to_string());
    }

    if is_external_idp_credentials(credentials) {
        return Some(enterprise_fallback_profile_arn(
            credentials.effective_api_region(config),
        ));
    }

    if is_social_credentials(credentials) {
        return Some(KIRO_SOCIAL_PROFILE_ARN.to_string());
    }

    if credentials.is_idc_refresh_credential()
        || credentials
            .provider
            .as_deref()
            .is_some_and(|value| compact_protocol_value(value) == "builderid")
    {
        return Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN.to_string());
    }

    None
}

pub fn resolve_agent_mode(credentials: &KiroCredentials, config: &Config) -> &'static str {
    match config.kiro_agent_mode_strategy {
        KiroAgentModeStrategy::Vibe => "vibe",
        KiroAgentModeStrategy::Spec => "spec",
        KiroAgentModeStrategy::Auto => {
            if credentials.is_api_key_credential()
                || credentials.is_idc_refresh_credential()
                || is_external_idp_credentials(credentials)
            {
                "vibe"
            } else if is_social_credentials(credentials) {
                "spec"
            } else {
                "vibe"
            }
        }
    }
}

pub fn extract_first_profile_arn(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;

    let profiles = value
        .get("profiles")
        .or_else(|| value.get("Profiles"))
        .and_then(Value::as_array)?;

    profiles.iter().find_map(|profile| {
        profile
            .get("arn")
            .or_else(|| profile.get("profileArn"))
            .or_else(|| profile.get("profileARN"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|arn| !arn.is_empty())
            .map(ToOwned::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn header_profile_skips_external_idp_fallback() {
        let mut credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            api_region: Some("eu-west-1".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_profile_arn(&credentials, &Config::default()), None);

        credentials.api_region = Some("us-east-1".to_string());
        assert_eq!(resolve_profile_arn(&credentials, &Config::default()), None);
    }

    #[test]
    fn real_profile_arn_wins_over_fallbacks() {
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_profile_arn(&credentials, &Config::default()).as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
    }

    #[test]
    fn recognizes_enterprise_provider_aliases() {
        for provider in [
            "Enterprise",
            "ExternalIdp",
            "External IDP",
            "external_idp",
            "IAM_SSO",
            "IAMSSO",
            "AWSIdC",
            "AWS_IDC",
            "Internal",
        ] {
            let credentials = KiroCredentials {
                auth_method: Some("idc".to_string()),
                provider: Some(provider.to_string()),
                api_region: Some("eu-west-1".to_string()),
                client_id: Some("client".to_string()),
                client_secret: Some("secret".to_string()),
                ..Default::default()
            };

            assert!(
                is_external_idp_credentials(&credentials),
                "provider alias {provider} should be treated as external IdP"
            );
            assert_eq!(
                resolve_profile_arn(&credentials, &Config::default()).as_deref(),
                None,
                "provider alias {provider} should not send fallback ARN to header/query APIs"
            );
        }
    }

    #[test]
    fn recognizes_enterprise_auth_method_aliases() {
        for auth_method in [
            "external-idp",
            "external IDP",
            "externalidp",
            "iam_sso",
            "IAMSSO",
            "aws-idc",
            "AWS_IDC",
            "Internal",
        ] {
            let credentials = KiroCredentials {
                auth_method: Some(auth_method.to_string()),
                client_id: Some("client".to_string()),
                client_secret: Some("secret".to_string()),
                ..Default::default()
            };

            assert!(
                is_external_idp_credentials(&credentials),
                "auth method alias {auth_method} should be treated as external IdP"
            );
        }
    }

    #[test]
    fn header_profile_skips_builder_id_placeholder_for_idc_credentials() {
        let credentials = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_profile_arn(&credentials, &Config::default()), None);
    }

    #[test]
    fn streaming_profile_keeps_builder_id_placeholder_for_idc_credentials() {
        let credentials = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_streaming_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
    }

    #[test]
    fn streaming_profile_uses_enterprise_fallback_without_persisting_it() {
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            api_region: Some("eu-west-1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_streaming_profile_arn(&credentials, &Config::default()).as_deref(),
            Some("arn:aws:codewhisperer:eu-central-1:610548660232:profile/VNECVYCYYAWN")
        );
    }

    #[test]
    fn persisted_enterprise_fallback_is_not_treated_as_real_header_profile() {
        let fallback = enterprise_fallback_profile_arn("us-east-1");
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            provider: Some("Enterprise".to_string()),
            profile_arn: Some(fallback.clone()),
            ..Default::default()
        };

        assert!(is_enterprise_fallback_profile_arn(&fallback));
        assert_eq!(resolve_profile_arn(&credentials, &Config::default()), None);
        assert_eq!(
            resolve_streaming_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(fallback.as_str())
        );
    }

    #[test]
    fn api_key_credentials_do_not_invent_builder_id_profile_arn() {
        let credentials = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_test".to_string()),
            provider: Some("Enterprise".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/STALE".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_profile_arn(&credentials, &Config::default()), None);
        assert!(!is_external_idp_credentials(&credentials));
        assert!(!is_social_credentials(&credentials));
    }

    #[test]
    fn extracts_first_profile_arn() {
        let body = r#"{"profiles":[{"profileName":"dev","arn":"arn:profile/1"}]}"#;
        assert_eq!(
            extract_first_profile_arn(body).as_deref(),
            Some("arn:profile/1")
        );
    }

    #[test]
    fn resolves_agent_mode_strategy() {
        let mut config = Config::default();
        let social = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        let idc = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let api_key = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_test".to_string()),
            ..Default::default()
        };

        assert_eq!(resolve_agent_mode(&social, &config), "vibe");

        config.kiro_agent_mode_strategy = KiroAgentModeStrategy::Auto;
        assert_eq!(resolve_agent_mode(&social, &config), "spec");
        assert_eq!(resolve_agent_mode(&idc, &config), "vibe");
        assert_eq!(resolve_agent_mode(&api_key, &config), "vibe");

        config.kiro_agent_mode_strategy = KiroAgentModeStrategy::Spec;
        assert_eq!(resolve_agent_mode(&idc, &config), "spec");
    }
}
