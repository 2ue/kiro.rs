//! Account runtime facade.
//!
//! The old implementation still lives behind the external-pool module while it
//! is being migrated. New integration points should depend on this account
//! runtime boundary instead of importing the legacy scheduler manager directly.

pub type AccountRuntimeManager = crate::external_pool::ExternalPoolManager;

pub type AccountRuntimeConfig = crate::model::config::ExternalPoolsConfig;
