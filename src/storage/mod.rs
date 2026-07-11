pub mod postgres;
pub mod redis_cache;

#[cfg(test)]
pub(crate) fn integration_test_url(variable: &'static str) -> Option<String> {
    match std::env::var(variable) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        Ok(_) | Err(_) if integration_tests_are_required() => {
            panic!("{variable} must be set when KIRO_RS_REQUIRE_STORAGE_TESTS=1")
        }
        Ok(_) | Err(_) => None,
    }
}

#[cfg(test)]
fn integration_tests_are_required() -> bool {
    std::env::var("KIRO_RS_REQUIRE_STORAGE_TESTS").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}
