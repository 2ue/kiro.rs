//! Claude Code execution compatibility.
//!
//! Anthropic exposes code execution as a server-side tool. Kiro only returns a normal
//! tool-use event, so this module executes the returned Python snippet locally when the
//! operator explicitly enables it and rewrites the response into Anthropic server-tool
//! blocks. The default is disabled.

use std::{
    fs,
    io::Read,
    process::{Child, Command, Stdio},
    sync::mpsc,
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
    if !config.enabled_for_route(endpoint) || tool_choice_is_none(request) {
        return None;
    }
    request
        .tools
        .as_ref()?
        .iter()
        .any(is_code_execution_tool)
        .then(|| config.clone())
}

/// `tool_choice: none` forbids every tool call this turn, so nothing may be executed.
fn tool_choice_is_none(request: &MessagesRequest) -> bool {
    request.tool_choice.as_ref().is_some_and(|choice| {
        choice.as_str() == Some("none")
            || choice.get("type").and_then(Value::as_str) == Some("none")
    })
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

/// A completed code execution tool call waiting to be run off the async runtime.
#[derive(Debug, Clone)]
pub(crate) struct PendingCodeExecution {
    pub(crate) input: Value,
    pub(crate) config: CodeExecutionConfig,
}

/// Runs [`execute`] on the blocking thread pool so a slow snippet never stalls an async worker.
pub(crate) async fn execute_blocking(input: Value, config: CodeExecutionConfig) -> ExecutionResult {
    match tokio::task::spawn_blocking(move || execute(&input, &config)).await {
        Ok(result) => result,
        Err(error) => failure(&format!("code execution task failed: {error}")),
    }
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
    // Own process group, so background children can be killed together with the leader.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failure(&format!("failed to start Python: {error}")),
    };
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let max_output = config.max_output_bytes.clamp(1, 1024 * 1024);
    let stdout_reader = spawn_reader(stdout, max_output);
    let stderr_reader = spawn_reader(stderr, max_output);

    let timeout = Duration::from_millis(config.timeout_ms.clamp(100, 120_000));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child_exited(&mut child) {
            Ok(true) => break,
            Ok(false) if Instant::now() >= deadline => {
                timed_out = true;
                break;
            }
            Ok(false) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                kill_process_tree(&mut child);
                let _ = child.wait();
                return failure(&format!("failed to wait for Python: {error}"));
            }
        }
    }
    // Kill the whole group whether the leader exited or timed out: a background child would
    // otherwise keep the pipes open and block the readers. The leader is still unreaped here, so
    // its pid (the group id) cannot have been reused.
    kill_process_tree(&mut child);
    let status = child.wait().ok();

    // A process that left the group (setsid) can still hold the pipes; never wait on it.
    let stdout = stdout_reader.recv_timeout(READER_GRACE).unwrap_or_default();
    let stderr = stderr_reader.recv_timeout(READER_GRACE).unwrap_or_default();
    let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
    if timed_out {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str("execution timed out");
    }
    let return_code = if timed_out {
        124
    } else {
        status.and_then(|status| status.code()).unwrap_or(1)
    };
    ExecutionResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
        return_code,
    }
}

/// How long to wait for the pipe readers after the process group has been killed.
const READER_GRACE: Duration = Duration::from_secs(1);

fn spawn_reader<R: Read + Send + 'static>(reader: R, max_output: usize) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_limited(reader, max_output));
    });
    receiver
}

/// Whether the child has exited, without reaping it.
#[cfg(unix)]
fn child_exited(child: &mut Child) -> std::io::Result<bool> {
    // SAFETY: siginfo_t is a plain C struct; all-zero is a valid value.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out pointer. WNOWAIT leaves the child waitable for `Child::wait`.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc == -1 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(error)
        };
    }
    // SAFETY: waitid filled `info` (or left it zeroed when nothing has exited yet).
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(not(unix))]
fn child_exited(child: &mut Child) -> std::io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

/// Kills the child's whole process group (unix) and the child itself.
fn kill_process_tree(child: &mut Child) {
    kill_process_group(child);
    let _ = child.kill();
}

#[cfg(unix)]
fn kill_process_group(child: &Child) {
    // The child was spawned with `process_group(0)`, so its pid is the group id.
    let Ok(pgid) = libc::pid_t::try_from(child.id()) else {
        return;
    };
    // SAFETY: killpg only sends a signal; ESRCH (group already gone) is ignored.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_child: &Child) {}

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

pub(crate) fn failure(message: &str) -> ExecutionResult {
    ExecutionResult {
        stdout: String::new(),
        stderr: message.to_string(),
        return_code: 1,
    }
}

/// Replaces ordinary `tool_use(name=code_execution)` blocks with the Anthropic server-tool
/// pair expected by Claude Code. Returns the number of handled blocks. Callers must apply the
/// response tool gate first so disallowed calls are never executed.
pub(crate) async fn rewrite_blocks(
    content: &mut Vec<Value>,
    config: &CodeExecutionConfig,
) -> usize {
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
        let result = execute_blocking(input.clone(), config.clone()).await;
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

    #[tokio::test]
    async fn response_uses_server_tool_blocks() {
        let mut content = vec![json!({
            "type": "tool_use",
            "id": "toolu_01abc",
            "name": "code_execution",
            "input": {"code": "print('ok')"}
        })];
        let count = rewrite_blocks(&mut content, &CodeExecutionConfig::default()).await;
        assert_eq!(count, 1);
        assert_eq!(content[0]["type"], "server_tool_use");
        assert_eq!(content[0]["name"], "bash_code_execution");
        assert_eq!(content[1]["type"], "bash_code_execution_tool_result");
        assert_eq!(content[1]["content"]["return_code"], 0);
    }

    fn timeout_config(timeout_ms: u64) -> CodeExecutionConfig {
        CodeExecutionConfig {
            timeout_ms,
            ..CodeExecutionConfig::default()
        }
    }

    #[cfg(unix)]
    fn process_alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks whether the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_background_children_that_hold_the_pipes() {
        let started = Instant::now();
        let result = execute(
            &json!({"command": "sleep 30 & echo $!; sleep 30"}),
            &timeout_config(300),
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "background child kept the pipes open for {:?}",
            started.elapsed()
        );
        assert_eq!(result.return_code, 124);
        assert!(result.stderr.contains("execution timed out"));
        let pid: libc::pid_t = result.stdout.trim().parse().expect("background pid");
        // The orphan is reaped by init shortly after SIGKILL.
        let gone_by = Instant::now() + Duration::from_secs(3);
        while process_alive(pid) && Instant::now() < gone_by {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !process_alive(pid),
            "background child {pid} survived the timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn finished_leader_does_not_wait_for_background_children() {
        let started = Instant::now();
        let result = execute(
            &json!({"command": "sleep 30 & printf LEADER_DONE"}),
            &timeout_config(10_000),
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "background child kept the pipes open for {:?}",
            started.elapsed()
        );
        assert_eq!(result.return_code, 0);
        assert_eq!(result.stdout, "LEADER_DONE");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_execution_does_not_stall_the_async_runtime() {
        let execution = tokio::spawn(execute_blocking(
            json!({"command": "sleep 1; printf SLOW_DONE"}),
            timeout_config(5_000),
        ));
        // On a current-thread runtime this timer only fires if execution left the worker.
        let ticked = tokio::time::timeout(Duration::from_millis(500), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;
        assert!(ticked.is_ok(), "the async worker was blocked by execution");
        let result = execution.await.expect("execution task");
        assert_eq!(result.stdout, "SLOW_DONE");
    }

    #[test]
    fn tool_choice_none_disables_execution_for_the_turn() {
        let request = |tool_choice: Value| -> MessagesRequest {
            serde_json::from_value(json!({
                "model": "claude-sonnet-4",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "run"}],
                "tools": [{"type": "code_execution_20250825", "name": "code_execution"}],
                "tool_choice": tool_choice
            }))
            .expect("request")
        };
        let config = CodeExecutionConfig {
            enabled: true,
            ..CodeExecutionConfig::default()
        };
        assert!(
            request_config(&request(json!({"type": "auto"})), "/v1/messages", &config).is_some()
        );
        assert!(
            request_config(&request(json!({"type": "none"})), "/v1/messages", &config).is_none()
        );
        assert!(request_config(&request(json!("none")), "/v1/messages", &config).is_none());
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
