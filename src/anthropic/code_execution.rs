//! Claude Code execution compatibility.
//!
//! Anthropic exposes code execution as a server-side tool. Kiro only returns a normal
//! tool-use event, so this module executes the returned Python snippet locally when the
//! operator explicitly enables it and rewrites the response into Anthropic server-tool
//! blocks. The default is disabled.

use std::{
    fs,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use uuid::Uuid;

use super::{envelope, types::MessagesRequest};
use crate::model::config::CodeExecutionConfig;

const CODE_EXECUTION_TOOL_TYPE_PREFIXES: &[&str] = &["code_execution_", "bash_code_execution_"];
const CODE_EXECUTION_TOOL_NAMES: &[&str] = &["code_execution", "bash_code_execution"];
const BASH_CODE_EXECUTION_NAME: &str = "bash_code_execution";

#[derive(Debug, Clone)]
pub(crate) struct ExecutionResult {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) return_code: i32,
}

/// Returns the route-specific execution config only when the request declared Anthropic's
/// native code execution tool. Ordinary tools named similarly are left untouched.
pub(crate) fn request_config(
    request: &MessagesRequest,
    endpoint: &str,
    config: &CodeExecutionConfig,
) -> Option<CodeExecutionConfig> {
    if !config.enabled_for_route(endpoint) {
        return None;
    }
    request
        .tools
        .as_ref()?
        .iter()
        .any(is_code_execution_tool)
        .then(|| config.clone())
}

fn is_code_execution_tool(tool: &super::types::Tool) -> bool {
    CODE_EXECUTION_TOOL_NAMES.contains(&tool.name.as_str())
        && tool.tool_type.as_deref().is_some_and(|kind| {
            CODE_EXECUTION_TOOL_TYPE_PREFIXES
                .iter()
                .any(|prefix| kind.starts_with(prefix))
        })
}

pub(crate) fn is_code_execution_name(name: &str) -> bool {
    CODE_EXECUTION_TOOL_NAMES.contains(&name)
}

pub(crate) fn execute(input: &Value, config: &CodeExecutionConfig) -> ExecutionResult {
    let root = std::env::temp_dir().join(format!("kiro-code-exec-{}", Uuid::new_v4()));
    if let Err(error) = fs::create_dir(&root) {
        return failure(&format!("failed to create execution directory: {error}"));
    }

    let result = if let Some(code) = input.get("code").and_then(Value::as_str) {
        let language = input
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("python");
        if code.is_empty() {
            failure("code_execution input is missing code")
        } else if code.len() > config.max_code_bytes.clamp(1, 1024 * 1024) {
            failure("code_execution input exceeds the configured size limit")
        } else if !language.eq_ignore_ascii_case("python")
            && !language.eq_ignore_ascii_case("python3")
        {
            failure("only Python code execution is enabled")
        } else {
            run_python(code, &root, config)
        }
    } else if let Some(command) = input.get("command").and_then(Value::as_str) {
        if command.is_empty() {
            failure("bash_code_execution input is missing command")
        } else if command.len() > config.max_code_bytes.clamp(1, 1024 * 1024) {
            failure("bash_code_execution input exceeds the configured size limit")
        } else {
            run_shell(command, &root, config)
        }
    } else {
        failure("code_execution input is missing code or command")
    };
    let _ = fs::remove_dir_all(&root);
    result
}

fn run_python(code: &str, cwd: &std::path::Path, config: &CodeExecutionConfig) -> ExecutionResult {
    let mut command = match python_command() {
        Some(command) => Command::new(command),
        None => return failure("Python interpreter is not available"),
    };
    run_process(&mut command, ["-I", "-S", "-c", code], cwd, config)
}

fn run_shell(
    command_line: &str,
    cwd: &std::path::Path,
    config: &CodeExecutionConfig,
) -> ExecutionResult {
    let mut command = Command::new("/bin/sh");
    run_process(&mut command, ["-c", command_line], cwd, config)
}

fn run_process<I, S>(
    command: &mut Command,
    args: I,
    cwd: &std::path::Path,
    config: &CodeExecutionConfig,
) -> ExecutionResult
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failure(&format!("failed to start Python: {error}")),
    };
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let max_output = config.max_output_bytes.clamp(1, 1024 * 1024);
    let stdout_reader = thread::spawn(move || read_limited(stdout, max_output));
    let stderr_reader = thread::spawn(move || read_limited(stderr, max_output));

    let timeout = Duration::from_millis(config.timeout_ms.clamp(100, 120_000));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return failure(&format!("failed to wait for Python: {error}"));
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
    if timed_out {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str("execution timed out");
    }
    let return_code = status
        .and_then(|status| status.code())
        .unwrap_or(if timed_out { 124 } else { 1 });
    ExecutionResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
        return_code,
    }
}

fn read_limited<R: Read>(mut reader: R, max_output: usize) -> Vec<u8> {
    let mut output = Vec::new();
    let mut limited = reader.by_ref().take(max_output as u64 + 1);
    let _ = limited.read_to_end(&mut output);
    if output.len() > max_output {
        output.truncate(max_output);
        output.extend_from_slice(b"\n[output truncated]");
    }
    output
}

fn python_command() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .output()
            .is_ok()
    })
}

fn failure(message: &str) -> ExecutionResult {
    ExecutionResult {
        stdout: String::new(),
        stderr: message.to_string(),
        return_code: 1,
    }
}

/// Replaces ordinary `tool_use(name=code_execution)` blocks with the Anthropic server-tool
/// pair expected by Claude Code. Returns the number of handled blocks.
pub(crate) fn rewrite_blocks(content: &mut Vec<Value>, config: &CodeExecutionConfig) -> usize {
    let mut rewritten = Vec::with_capacity(content.len());
    let mut count = 0;
    for block in std::mem::take(content) {
        let is_tool = block.get("type").and_then(Value::as_str) == Some("tool_use")
            && block
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(is_code_execution_name);
        if !is_tool {
            rewritten.push(block);
            continue;
        }
        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
        let server_id = envelope::server_tool_use_id();
        let result = execute(&input, config);
        rewritten.push(json!({
            "type": "server_tool_use",
            "id": server_id,
            "name": BASH_CODE_EXECUTION_NAME,
            "input": input,
        }));
        rewritten.push(result_block(&server_id, &result));
        count += 1;
    }
    *content = rewritten;
    count
}

pub(crate) fn result_block(tool_use_id: &str, result: &ExecutionResult) -> Value {
    json!({
        "type": "bash_code_execution_tool_result",
        "tool_use_id": tool_use_id,
        "content": {
            "type": "bash_code_execution_result",
            "stdout": result.stdout,
            "stderr": result.stderr,
            "return_code": result.return_code,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types;

    #[test]
    fn python_execution_returns_stdout() {
        let result = execute(
            &json!({"code": "print('HELLO_CHECK')", "language": "python"}),
            &CodeExecutionConfig::default(),
        );
        assert_eq!(result.return_code, 0);
        assert_eq!(result.stdout.trim(), "HELLO_CHECK");
    }

    #[test]
    fn bash_execution_returns_stdout() {
        let result = execute(
            &json!({"command": "printf HELLO_BASH", "language": "bash"}),
            &CodeExecutionConfig::default(),
        );
        assert_eq!(result.return_code, 0);
        assert_eq!(result.stdout, "HELLO_BASH");
    }

    #[test]
    fn response_uses_server_tool_blocks() {
        let mut content = vec![json!({
            "type": "tool_use",
            "id": "toolu_01abc",
            "name": "code_execution",
            "input": {"code": "print('ok')"}
        })];
        let count = rewrite_blocks(&mut content, &CodeExecutionConfig::default());
        assert_eq!(count, 1);
        assert_eq!(content[0]["type"], "server_tool_use");
        assert_eq!(content[0]["name"], "bash_code_execution");
        assert_eq!(content[1]["type"], "bash_code_execution_tool_result");
        assert_eq!(content[1]["content"]["return_code"], 0);
    }

    #[test]
    fn recognizes_legacy_and_bash_server_tool_declarations_only() {
        let legacy = types::Tool {
            tool_type: Some("code_execution_20250522".to_string()),
            name: "code_execution".to_string(),
            description: String::new(),
            input_schema: Default::default(),
            max_uses: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
        };
        let bash = types::Tool {
            tool_type: Some("bash_code_execution_20250825".to_string()),
            name: "bash_code_execution".to_string(),
            ..legacy.clone()
        };
        let ordinary = types::Tool {
            tool_type: Some("custom_tool_20250825".to_string()),
            name: "bash_code_execution".to_string(),
            ..legacy.clone()
        };
        assert!(is_code_execution_tool(&legacy));
        assert!(is_code_execution_tool(&bash));
        assert!(!is_code_execution_tool(&ordinary));
    }
}
