//! Account-scoped identity profile used by upstream requests and Admin diagnostics.
//!
//! This profile deliberately separates protocol identity from scheduler health. It never
//! contains bearer tokens, refresh tokens, API keys, proxy credentials, or a raw profile ARN.

use sha2::{Digest, Sha256};

use crate::kiro::machine_id::{
    MACHINE_ID_DERIVATION_VERSION, MachineIdSource, resolve_from_credentials,
};
use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::{Config, TlsBackend};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountIdentityProfile {
    pub credential_id: Option<u64>,
    pub machine_id: String,
    pub machine_id_hash: String,
    pub machine_id_source: MachineIdSource,
    pub fingerprint_version: &'static str,
    pub auth_method: Option<String>,
    pub provider: Option<String>,
    pub auth_region: String,
    pub api_region: String,
    pub endpoint: String,
    pub proxy_resource_id: Option<u64>,
    pub ua_profile: &'static str,
    pub tls_profile: &'static str,
}

impl AccountIdentityProfile {
    pub fn from_credentials(credentials: &KiroCredentials, config: &Config) -> Self {
        let resolution = resolve_from_credentials(credentials, config);
        let endpoint = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&config.default_endpoint)
            .to_string();
        let (ua_profile, tls_profile) = match endpoint.as_str() {
            "cli" => (
                "kiro-cli-rust-1.3.15-streaming-0.1.16551",
                tls_profile_name(config.tls_backend),
            ),
            _ => (
                "kiro-ide-js-1.0.34-codewhispererstreaming",
                tls_profile_name(config.tls_backend),
            ),
        };

        Self {
            credential_id: credentials.id,
            machine_id_hash: hash_machine_id(&resolution.machine_id),
            machine_id: resolution.machine_id,
            machine_id_source: resolution.source,
            fingerprint_version: MACHINE_ID_DERIVATION_VERSION,
            auth_method: credentials.auth_method.clone(),
            provider: credentials.provider.clone(),
            auth_region: credentials.effective_auth_region(config).to_string(),
            api_region: credentials.effective_api_region(config).to_string(),
            endpoint,
            proxy_resource_id: credentials.proxy_resource_id,
            ua_profile,
            tls_profile,
        }
    }
}

pub fn hash_machine_id(machine_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kiro-machine-id-observation-v1");
    hasher.update([0]);
    hasher.update(machine_id.as_bytes());
    hex::encode(hasher.finalize())
}

pub const fn tls_profile_name(backend: TlsBackend) -> &'static str {
    match backend {
        TlsBackend::Rustls => "rustls",
        TlsBackend::NativeTls => "native-tls",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_account_scoped_without_exposing_secret_material() {
        let mut credentials = KiroCredentials {
            id: Some(7),
            refresh_token: Some("refresh-secret".to_string()),
            auth_method: Some("social".to_string()),
            provider: Some("Github".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let profile = AccountIdentityProfile::from_credentials(&credentials, &config);
        assert_eq!(profile.credential_id, Some(7));
        assert_eq!(
            profile.machine_id_source,
            MachineIdSource::DerivedRefreshToken
        );
        assert_eq!(profile.endpoint, "ide");
        assert_eq!(
            profile.ua_profile,
            "kiro-ide-js-1.0.34-codewhispererstreaming"
        );
        assert!(!profile.machine_id_hash.contains("refresh-secret"));
        assert!(!profile.machine_id_hash.contains(&profile.machine_id));

        credentials.refresh_token = Some("other-secret".to_string());
        let other = AccountIdentityProfile::from_credentials(&credentials, &config);
        assert_ne!(profile.machine_id_hash, other.machine_id_hash);
    }
}
