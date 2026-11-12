//! Shared Kiro upstream protocol helpers.

use serde_json::Value;

use crate::kiro::model::credentials::{DeclaredAuthKind, KiroCredentials};
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

/// Whether Kiro API calls carry the external IdP semantics (`TokenType: EXTERNAL_IDP`, real
/// profile ARN). An explicit `authMethod` decides; `provider` is only consulted when
/// `authMethod` is missing or unrecognised.
pub fn is_external_idp_credentials(credentials: &KiroCredentials) -> bool {
    credentials.is_external_idp_refresh_credential()
}

pub fn is_social_credentials(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }
    match credentials.declared_auth_kind() {
        Some(kind) => kind == DeclaredAuthKind::Social,
        None => credentials.provider.as_deref().is_some_and(|value| {
            matches!(compact_protocol_value(value).as_str(), "github" | "google")
        }),
    }
}

/// Recognize Builder ID credentials even when an importer only preserved `provider`.
pub fn is_builder_id_credentials(credentials: &KiroCredentials) -> bool {
    if is_external_idp_credentials(credentials) {
        return false;
    }
    if credentials.is_idc_refresh_credential() {
        return true;
    }
    credentials.declared_auth_kind().is_none()
        && credentials
            .provider
            .as_deref()
            .is_some_and(|value| compact_protocol_value(value) == "builderid")
}

/// Resolve a real profile ARN for endpoints where the ARN is an optional selector.
///
/// MCP and usage-limit operations have their own credential-specific requirements.
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

/// Resolve the profile ARN required by the usage-limits control-plane endpoint.
///
/// BuilderId/IdC accounts use the protocol placeholder ARN for this endpoint.
/// Model-list requests have a separate optional-profile rule; MCP also requires
/// this placeholder for Builder ID requests.
pub fn resolve_usage_limits_profile_arn(
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
        .filter(|arn| !arn.is_empty() && is_real_profile_arn(arn))
    {
        return Some(profile_arn.to_string());
    }

    if is_builder_id_credentials(credentials) {
        return Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN.to_string());
    }

    resolve_profile_arn(credentials, config)
}

/// Resolve the Profile ARN required by the Kiro MCP endpoint.
///
/// Builder ID requests require the protocol placeholder.
pub fn resolve_mcp_profile_arn(credentials: &KiroCredentials, config: &Config) -> Option<String> {
    resolve_usage_limits_profile_arn(credentials, config)
}

/// Resolve the Profile ARN sent with `ListAvailableModels`.
///
/// With `KiroIDE-1.4.x` user agents the endpoint rejects Builder ID/IdC requests that omit
/// `profileArn` (`400 Invalid profileArn.`, FREE IdC 2026-11-11); `0.11.107` still accepted
/// them. The placeholder is accepted by both versions, matching kiro-manager-lite.
pub fn resolve_models_profile_arn(
    credentials: &KiroCredentials,
    config: &Config,
) -> Option<String> {
    resolve_usage_limits_profile_arn(credentials, config)
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

    if is_builder_id_credentials(credentials) {
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

/// Whether `ListAvailableProfiles` should be used to fill a missing profile ARN.
///
/// External IdP always lacks a usable ARN until discovered. IdC covers both Builder ID and
/// organization (IAM Identity Center) accounts, which import identically; only organization
/// accounts have a profile. An IdC account that already stores the Builder ID placeholder was
/// confirmed as Builder ID by an earlier 403 and is not queried again.
pub fn profile_arn_discovery_applies(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }
    let stored = credentials
        .profile_arn
        .as_deref()
        .map(str::trim)
        .filter(|arn| !arn.is_empty());
    if is_external_idp_credentials(credentials) {
        return stored.is_none_or(|arn| !is_real_profile_arn(arn));
    }
    let declared_builder_id = credentials
        .provider
        .as_deref()
        .is_some_and(|value| compact_protocol_value(value) == "builderid");
    credentials.is_idc_refresh_credential() && stored.is_none() && !declared_builder_id
}

/// Whether a `ListAvailableProfiles` 403 body says the account has no profile access, as opposed
/// to an invalid or expired token.
pub fn profile_listing_denied_for_builder_id(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("builder id") || lower.contains("not authorized to access this feature")
}

/// Pick the profile in the account's API Region when the organization has several.
pub fn extract_preferred_profile_arn(body: &str, region: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let arns = value
        .get("profiles")
        .or_else(|| value.get("Profiles"))
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|profile| {
            profile
                .get("arn")
                .or_else(|| profile.get("profileArn"))
                .or_else(|| profile.get("profileARN"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|arn| !arn.is_empty())
        })
        .collect::<Vec<_>>();
    let region_marker = format!(":{region}:");
    arns.iter()
        .find(|arn| arn.contains(&region_marker))
        .or_else(|| arns.first())
        .map(|arn| (*arn).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn profile_discovery_targets_unconfirmed_idc_and_external_idp_only() {
        let idc = |profile_arn: Option<&str>| KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            profile_arn: profile_arn.map(str::to_string),
            ..Default::default()
        };
        assert!(profile_arn_discovery_applies(&idc(None)));
        let declared = KiroCredentials {
            provider: Some("BuilderId".to_string()),
            ..idc(None)
        };
        assert!(!profile_arn_discovery_applies(&declared));
        assert!(!profile_arn_discovery_applies(&idc(Some(
            KIRO_BUILDER_ID_PLACEHOLDER_ARN
        ))));
        assert!(!profile_arn_discovery_applies(&idc(Some(
            "arn:aws:codewhisperer:us-east-1:614333540591:profile/REAL"
        ))));

        let external = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            profile_arn: Some(enterprise_fallback_profile_arn("us-east-1")),
            ..Default::default()
        };
        assert!(profile_arn_discovery_applies(&external));

        let social = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        assert!(!profile_arn_discovery_applies(&social));
    }

    #[test]
    fn builder_id_profile_denial_is_distinguished_from_token_errors() {
        assert!(profile_listing_denied_for_builder_id(
            r#"{"message":"User is not authorized to access this feature.","reason":null}"#
        ));
        assert!(profile_listing_denied_for_builder_id(
            r#"{"message":"AWS Builder ID is not supported for this operation."}"#
        ));
        assert!(!profile_listing_denied_for_builder_id(
            r#"{"message":"Invalid token","reason":null}"#
        ));
    }

    #[test]
    fn preferred_profile_matches_the_account_region() {
        let body = r#"{"profiles":[
            {"arn":"arn:aws:codewhisperer:us-east-1:111111111111:profile/US"},
            {"arn":"arn:aws:codewhisperer:eu-central-1:111111111111:profile/EU"}
        ]}"#;
        assert_eq!(
            extract_preferred_profile_arn(body, "eu-central-1").as_deref(),
            Some("arn:aws:codewhisperer:eu-central-1:111111111111:profile/EU")
        );
        assert_eq!(
            extract_preferred_profile_arn(body, "ap-south-2").as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:111111111111:profile/US")
        );
    }

    #[test]
    fn model_list_profile_uses_builder_placeholder_for_idc() {
        let config = Config::default();
        let idc = KiroCredentials {
            auth_method: Some("idc".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_models_profile_arn(&idc, &config).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
        let social = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_models_profile_arn(&social, &config).as_deref(),
            Some(KIRO_SOCIAL_PROFILE_ARN)
        );
        let api_key = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_test".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_models_profile_arn(&api_key, &config), None);
    }

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
    fn explicit_idc_auth_method_is_not_reclassified_by_enterprise_provider() {
        // KAM / kiro-manager-lite / Kiro-Go export organization IdC accounts as
        // authMethod=IdC + clientId/clientSecret + provider=Enterprise (or an alias).
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
                auth_method: Some("IdC".to_string()),
                provider: Some(provider.to_string()),
                api_region: Some("eu-west-1".to_string()),
                client_id: Some("client".to_string()),
                client_secret: Some("secret".to_string()),
                ..Default::default()
            };

            assert!(
                !is_external_idp_credentials(&credentials),
                "provider {provider} must not turn an explicit IdC account into external IdP"
            );
            assert!(
                credentials.is_idc_refresh_credential(),
                "provider {provider}"
            );
            assert!(
                is_builder_id_credentials(&credentials),
                "provider {provider}"
            );
            assert!(!is_social_credentials(&credentials), "provider {provider}");
            assert_eq!(
                resolve_usage_limits_profile_arn(&credentials, &Config::default()).as_deref(),
                Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN),
                "provider {provider}"
            );
        }
    }

    #[test]
    fn enterprise_provider_aliases_imply_external_idp_only_without_auth_method() {
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
                provider: Some(provider.to_string()),
                api_region: Some("eu-west-1".to_string()),
                client_id: Some("client".to_string()),
                token_endpoint: Some("https://login.example.invalid/token".to_string()),
                ..Default::default()
            };

            assert!(
                is_external_idp_credentials(&credentials),
                "provider alias {provider} should be treated as external IdP"
            );
            assert!(
                !credentials.is_idc_refresh_credential(),
                "provider {provider}"
            );
            assert_eq!(
                resolve_profile_arn(&credentials, &Config::default()).as_deref(),
                None,
                "provider alias {provider} should not send fallback ARN to header/query APIs"
            );
        }
    }

    #[test]
    fn explicit_social_auth_method_ignores_enterprise_provider() {
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            provider: Some("Enterprise".to_string()),
            ..Default::default()
        };
        assert!(!is_external_idp_credentials(&credentials));
        assert!(is_social_credentials(&credentials));
        assert_eq!(
            resolve_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_SOCIAL_PROFILE_ARN)
        );
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
    fn usage_limits_profile_keeps_builder_id_placeholder_for_idc_credentials() {
        let credentials = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            profile_arn: Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN.to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_usage_limits_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
    }

    #[test]
    fn usage_limits_profile_falls_back_to_builder_id_placeholder_when_missing() {
        let credentials = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_usage_limits_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
    }

    #[test]
    fn mcp_profile_uses_builder_id_placeholder_while_optional_profile_omits_it() {
        let credentials = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_profile_arn(&credentials, &Config::default()), None);
        assert_eq!(
            resolve_mcp_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
    }

    #[test]
    fn mcp_profile_resolution_covers_authentication_matrix() {
        let real_arn = "arn:aws:codewhisperer:us-east-1:123456789012:profile/REAL";
        let cases = [
            (
                "aws sso iam auth method",
                KiroCredentials {
                    auth_method: Some("iam".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN),
            ),
            (
                "idc auth method",
                KiroCredentials {
                    auth_method: Some("idc".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN),
            ),
            (
                "builderid auth method",
                KiroCredentials {
                    auth_method: Some("builderid".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN),
            ),
            (
                "external idp without profile",
                KiroCredentials {
                    auth_method: Some("external_idp".to_string()),
                    provider: Some("Enterprise".to_string()),
                    ..Default::default()
                },
                None,
            ),
            (
                "external idp with real profile",
                KiroCredentials {
                    auth_method: Some("external_idp".to_string()),
                    provider: Some("Enterprise".to_string()),
                    profile_arn: Some(real_arn.to_string()),
                    ..Default::default()
                },
                Some(real_arn),
            ),
            (
                "api key ignores stale profile",
                KiroCredentials {
                    auth_method: Some("api_key".to_string()),
                    kiro_api_key: Some("ksk_test".to_string()),
                    provider: Some("Enterprise".to_string()),
                    profile_arn: Some(real_arn.to_string()),
                    ..Default::default()
                },
                None,
            ),
        ];

        for (label, credentials, expected) in cases {
            assert_eq!(
                resolve_mcp_profile_arn(&credentials, &Config::default()).as_deref(),
                expected,
                "unexpected MCP profile resolution for {label}"
            );
        }
    }

    #[test]
    fn provider_only_builder_id_keeps_required_usage_and_mcp_profile() {
        let credentials = KiroCredentials {
            provider: Some("BuilderId".to_string()),
            ..Default::default()
        };
        assert!(is_builder_id_credentials(&credentials));
        assert_eq!(
            resolve_usage_limits_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
        assert_eq!(
            resolve_mcp_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
        assert_eq!(
            resolve_streaming_profile_arn(&credentials, &Config::default()).as_deref(),
            Some(KIRO_BUILDER_ID_PLACEHOLDER_ARN)
        );
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
    fn extracts_profile_arn_from_profile_list() {
        let body = r#"{"profiles":[{"profileName":"dev","arn":"arn:profile/1"}]}"#;
        assert_eq!(
            extract_preferred_profile_arn(body, "us-east-1").as_deref(),
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
