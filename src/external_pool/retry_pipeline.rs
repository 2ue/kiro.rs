use super::*;

pub(super) fn should_retry_same_account(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolError,
) -> bool {
    if !err.retryable {
        return false;
    }
    // These errors should leave the current candidate for this request instead
    // of repeatedly sending to the same account. They are still treated as
    // recoverable health signals by the account scheduler.
    if err.auto_disable_reason.is_some()
        || err
            .cooldown
            .as_ref()
            .is_some_and(|(_, reason)| reason == "model_unavailable")
    {
        return false;
    }
    let Some(status) = err.status else {
        return false;
    };
    retry_status_matches(status, &config.same_account_retry_status_codes())
}

pub(super) fn same_account_retry_limit(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolError,
) -> u32 {
    if !should_retry_same_account(config, err) {
        return 0;
    }
    // Keep same-account replay bounded to a single retry. More than one retry on
    // the same account tends to amplify one upstream fault into repeated
    // failures for the same request, while the scheduler-level transient
    // penalty already handles future requests.
    config.external_pool_same_pool_retry_count.min(1)
}

pub(super) fn should_retry_cross_account(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolError,
) -> bool {
    if !err.retryable {
        return false;
    }
    if err.auto_disable_reason.is_some() {
        return true;
    }
    if err.protocol_error.is_some() {
        return config.external_pool_retry_on_protocol_error;
    }
    let Some(status) = err.status else {
        return config.external_pool_retry_on_network_error;
    };
    retry_status_matches(status, &config.cross_account_retry_status_codes())
}

pub(super) fn same_account_retry_delay(config: &ExternalPoolsConfig) -> Option<Duration> {
    let delay_ms = config.external_pool_same_pool_retry_delay_ms;
    (delay_ms > 0).then(|| Duration::from_millis(delay_ms))
}

fn retry_status_matches(status: StatusCode, configured: &std::collections::BTreeSet<u16>) -> bool {
    let code = status.as_u16();
    configured.contains(&code) || (status.is_server_error() && configured.contains(&500))
}
