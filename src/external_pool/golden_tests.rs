use super::tests::{projection_context_with_output_uplift, test_pool, test_route};
use super::*;
use crate::model::config::{ReportedUsageFieldPolicy, ReportedUsagePathPolicy};
use serde_json::Value;
use std::{fs, path::Path};

const STRATEGIES: [&str; 6] = [
    "no_cache",
    "current_high_cache",
    "kiro_rs_tool",
    "stable_segment_cache",
    "kiro_rs_tool_plus",
    "bounded_high_cache",
];

static GOLDEN_FILES_LOCK: SyncMutex<()> = SyncMutex::new(());

#[derive(Deserialize)]
struct GoldenCase {
    strategy: PromptCacheStrategyType,
    stream: bool,
    upstream_cache_read_evidence: bool,
    upstream_1h_write: bool,
    seed: u64,
    request: GoldenRequest,
    upstream_usage: Value,
    expected: Value,
}

#[derive(Deserialize)]
struct GoldenRequest {
    model: String,
    system_repeat: usize,
    user_text: String,
}

impl GoldenCase {
    fn strategy_name(&self) -> String {
        serde_json::to_value(self.strategy)
            .expect("serialize strategy")
            .as_str()
            .expect("strategy name")
            .to_string()
    }

    fn file_name(&self) -> String {
        format!(
            "{}__{}__{}__{}.json",
            self.strategy_name(),
            if self.stream { "stream" } else { "nonstream" },
            if self.upstream_cache_read_evidence {
                "evidence"
            } else {
                "noevidence"
            },
            if self.upstream_1h_write { "1h" } else { "no1h" },
        )
    }
}

fn golden_case_paths() -> Vec<PathBuf> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/usage");
    let mut paths: Vec<_> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("golden directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    paths
}

fn read_case(path: &Path) -> Result<(Value, GoldenCase), String> {
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let document: Value = serde_json::from_str(&contents).map_err(|error| error.to_string())?;
    let case = serde_json::from_value(document.clone()).map_err(|error| error.to_string())?;
    Ok((document, case))
}

fn usage_snapshot(usage: CacheUsage) -> Value {
    json!({
        "total_input_tokens": usage.total_input_tokens,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_input_tokens,
        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        "cache_creation": {
            "ephemeral_5m_input_tokens": usage.cache_creation_5m_input_tokens,
            "ephemeral_1h_input_tokens": usage.cache_creation_1h_input_tokens,
        },
    })
}

fn run_case(case: &GoldenCase) -> Value {
    let mut route = test_route(&case.request.model);
    let payload = route.payload.as_mut().expect("test payload");
    payload.stream = case.stream;
    payload.system.as_mut().expect("test system")[0].text =
        "You are a careful coding assistant. ".repeat(case.request.system_repeat);
    payload.messages[0].content = json!(case.request.user_text);
    route.request_input_tokens = count_external_route_input_tokens(payload);
    route.prompt_cache_strategy_type = case.strategy;
    route.prompt_cache_simulation_mode = if case.strategy.uses_high_cache_simulation() {
        PromptCacheSimulationMode::HighCache
    } else {
        PromptCacheSimulationMode::Disabled
    };
    route.reported_usage.default.enabled = true;
    route.reported_usage.default = ReportedUsagePathPolicy {
        enabled: true,
        input: ReportedUsageFieldPolicy::sample_input_max(96),
        cache_creation: ReportedUsageFieldPolicy::sample_target_with_multiplier(3_000, 1.2),
        ..ReportedUsagePathPolicy::default()
    };
    route.reset_preparation_cache();

    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    // 同时固定模板采样、plus 和 bounded 的线程局部随机源，避免影响其他测试。
    let previous_seed = fastrand::get_seed();
    fastrand::seed(case.seed);
    let context = projection_context_with_output_uplift(&route, &pool, 25, true, 10, 1_000, 50);
    let actual = if case.stream {
        run_stream_case(case, context.as_ref())
    } else {
        run_non_stream_case(case, context.as_ref())
    };
    fastrand::seed(previous_seed);
    actual
}

fn run_stream_case(case: &GoldenCase, context: Option<&ExternalUsageProjectionContext>) -> Value {
    let mut start_usage = case.upstream_usage.clone();
    start_usage["output_tokens"] = json!(0);
    let mut message_start = json!({
        "type": "message_start",
        "message": {
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": case.request.model,
            "usage": start_usage,
        },
    });
    process_usage_slots_in_sse_value(&mut message_start, context, None, true);

    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let mut message_delta = json!({
        "type": "message_delta",
        "delta": { "stop_reason": "end_turn", "stop_sequence": null },
        "usage": case.upstream_usage,
    });
    process_usage_slots_in_sse_value(&mut message_delta, context, Some(&capture), true);
    let capture = capture.lock();
    json!({
        "projected": capture.projected,
        "message_start_usage": message_start["message"]["usage"],
        "reported": message_delta["usage"],
        "shaped": if capture.projected {
            usage_snapshot(capture.shaped.expect("projected shaped usage"))
        } else {
            message_delta["usage"].clone()
        },
    })
}

fn run_non_stream_case(
    case: &GoldenCase,
    context: Option<&ExternalUsageProjectionContext>,
) -> Value {
    let mut response = json!({
        "id": "msg_x",
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "text", "text": "ok" }],
        "model": case.request.model,
        "usage": case.upstream_usage,
    });
    let projected = project_usage_value(&mut response["usage"], context, true, None);
    match projected {
        Some(projected) => json!({
            "projected": true,
            "reported": projected.reported.to_anthropic_usage_json(),
            "shaped": usage_snapshot(projected.shaped),
        }),
        None => json!({
            "projected": false,
            "reported": response["usage"],
            "shaped": response["usage"],
        }),
    }
}

fn collect_differences(
    file_name: &str,
    field: &str,
    expected: Option<&Value>,
    actual: Option<&Value>,
    differences: &mut Vec<String>,
) {
    if expected == actual {
        return;
    }
    match (expected, actual) {
        (Some(Value::Object(expected)), Some(Value::Object(actual))) => {
            let keys: std::collections::BTreeSet<_> =
                expected.keys().chain(actual.keys()).collect();
            for key in keys {
                collect_differences(
                    file_name,
                    &format!("{field}.{key}"),
                    expected.get(key),
                    actual.get(key),
                    differences,
                );
            }
        }
        (Some(Value::Array(expected)), Some(Value::Array(actual))) => {
            for index in 0..expected.len().max(actual.len()) {
                collect_differences(
                    file_name,
                    &format!("{field}[{index}]"),
                    expected.get(index),
                    actual.get(index),
                    differences,
                );
            }
        }
        _ => differences.push(format!(
            "{file_name}: {field}: expected={}, actual={}",
            expected
                .map(Value::to_string)
                .unwrap_or_else(|| "<missing>".to_string()),
            actual
                .map(Value::to_string)
                .unwrap_or_else(|| "<missing>".to_string()),
        )),
    }
}

#[test]
fn golden_usage_snapshots() {
    let _guard = GOLDEN_FILES_LOCK.lock();
    let update = std::env::var("GOLDEN_UPDATE").is_ok_and(|value| value == "1");
    let paths = golden_case_paths();
    assert!(!paths.is_empty(), "no golden usage cases found");
    let mut differences = Vec::new();
    for path in paths {
        let file_name = path.file_name().expect("case file name").to_string_lossy();
        let (mut document, case) = match read_case(&path) {
            Ok(case) => case,
            Err(error) => {
                differences.push(format!("{file_name}: invalid case: {error}"));
                continue;
            }
        };
        let actual = run_case(&case);
        if update {
            document["expected"] = actual;
            let contents = serde_json::to_string_pretty(&document).expect("serialize golden case");
            if let Err(error) = fs::write(&path, format!("{contents}\n")) {
                differences.push(format!("{file_name}: write failed: {error}"));
            }
        } else {
            collect_differences(
                &file_name,
                "expected",
                Some(&case.expected),
                Some(&actual),
                &mut differences,
            );
        }
    }
    assert!(
        differences.is_empty(),
        "golden usage differences ({}):\n{}",
        differences.len(),
        differences.join("\n"),
    );
}

#[test]
fn golden_usage_case_count() {
    let _guard = GOLDEN_FILES_LOCK.lock();
    let paths = golden_case_paths();
    assert!(
        paths.len() >= 48,
        "expected >= 48 cases, got {}",
        paths.len()
    );
    let mut counts = BTreeMap::new();
    let mut combinations = HashSet::new();
    for path in paths {
        let (_, case) =
            read_case(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let strategy = case.strategy_name();
        assert_eq!(
            path.file_name().expect("case file name").to_string_lossy(),
            case.file_name(),
            "case file name must describe its matrix combination",
        );
        let creation_tokens = if case.upstream_1h_write { 3_000 } else { 0 };
        assert_eq!(
            case.upstream_usage,
            json!({
                "input_tokens": 1_200,
                "output_tokens": 350,
                "cache_read_input_tokens": if case.upstream_cache_read_evidence { 8_000 } else { 0 },
                "cache_creation_input_tokens": creation_tokens,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 0,
                    "ephemeral_1h_input_tokens": creation_tokens,
                },
            }),
            "{}: upstream usage must match its matrix flags",
            path.display(),
        );
        assert!(
            combinations.insert((
                strategy.clone(),
                case.stream,
                case.upstream_cache_read_evidence,
                case.upstream_1h_write,
            )),
            "duplicate matrix combination: {}",
            path.display(),
        );
        *counts.entry(strategy).or_insert(0) += 1;
    }
    for strategy in STRATEGIES {
        assert_eq!(
            counts.get(strategy),
            Some(&8),
            "{strategy} must have 8 cases"
        );
    }
}
