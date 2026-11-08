#!/usr/bin/env python3
"""Debug hot-reload loop for the isolated kirotest instance.

The loop watches backend sources, performs an incremental debug build in a
persistent target directory, and restarts only the 59139 process after a
successful build.  It never contacts or mutates the 59138 production service.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Iterable


REPO = Path(__file__).resolve().parents[4]
DEFAULT_ROOT = Path(os.environ.get("KIROTEST_DEV_ROOT", "/tmp/kirotest-dev"))
DEFAULT_CONFIG = DEFAULT_ROOT / "config" / "config.json"
DEFAULT_CREDENTIALS = DEFAULT_ROOT / "config" / "credentials.json"
DEV_CACHE = Path.home() / ".cache" / "kirotest-dev"
DEFAULT_BINARY = DEV_CACHE / "kiro-rs-dev"
DEFAULT_TARGET = DEV_CACHE / "cargo-target-debug"
DEFAULT_PID = DEFAULT_ROOT / "dev-loop.pid"
DEFAULT_LOG = DEFAULT_ROOT / "logs" / "dev-loop.log"

WATCH_ROOTS = ("Cargo.toml", "Cargo.lock", "build.rs", ".cargo", "src")
WATCH_SUFFIXES = {".rs", ".toml", ".lock"}
IGNORE_DIRS = {"target", ".git", ".codex-artifacts", "node_modules"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--once", action="store_true", help="构建并重启一次，然后退出")
    parser.add_argument(
        "--check",
        action="store_true",
        help="只检查资源、构建并复制二进制，不停止或启动 59139",
    )
    parser.add_argument(
        "--adopt-existing",
        action="store_true",
        help="允许接管当前命令行匹配的 59139 进程；默认只管理自己的 PID 文件",
    )
    parser.add_argument("--poll-seconds", type=float, default=1.0)
    parser.add_argument("--jobs", type=int, default=int(os.environ.get("KIROTEST_BUILD_JOBS", "2")))
    parser.add_argument(
        "--pause-memory-gib",
        type=float,
        default=float(os.environ.get("KIROTEST_PAUSE_MEMORY_GIB", "4")),
        help="编译过程中低于此可用内存时暂停 cargo",
    )
    parser.add_argument(
        "--resume-memory-gib",
        type=float,
        default=float(os.environ.get("KIROTEST_RESUME_MEMORY_GIB", "6")),
        help="资源恢复到此可用内存后继续 cargo",
    )
    parser.add_argument(
        "--pause-swap-gib",
        type=float,
        default=float(os.environ.get("KIROTEST_PAUSE_SWAP_GIB", "8")),
        help="编译过程中低于此可用 swap 时暂停 cargo",
    )
    parser.add_argument(
        "--resume-swap-gib",
        type=float,
        default=float(os.environ.get("KIROTEST_RESUME_SWAP_GIB", "10")),
        help="资源恢复到此可用 swap 后继续 cargo",
    )
    parser.add_argument(
        "--pause-load",
        type=float,
        default=float(os.environ.get("KIROTEST_PAUSE_LOAD", "8")),
        help="1 分钟 load 超过此值时暂停 cargo",
    )
    parser.add_argument(
        "--resume-load",
        type=float,
        default=float(os.environ.get("KIROTEST_RESUME_LOAD", "6")),
        help="1 分钟 load 降到此值后继续 cargo",
    )
    parser.add_argument("--port", type=int, default=int(os.environ.get("KIROTEST_DEV_PORT", "59139")))
    parser.add_argument(
        "--prod-port",
        type=int,
        default=int(os.environ.get("KIROTEST_PROD_PORT", "59138")),
        help="生产实例只读健康探针端口",
    )
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--credentials", type=Path, default=DEFAULT_CREDENTIALS)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--target-dir", type=Path, default=DEFAULT_TARGET)
    parser.add_argument("--pid-file", type=Path, default=DEFAULT_PID)
    parser.add_argument("--log-file", type=Path, default=DEFAULT_LOG)
    return parser.parse_args()


def log(message: str, *, file=None) -> None:
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    print(f"[{stamp}] {message}", file=file or sys.stderr, flush=True)


def available_memory_kib() -> int:
    values: dict[str, int] = {}
    with Path("/proc/meminfo").open() as handle:
        for line in handle:
            name, value, *_ = line.split()
            values[name.rstrip(":")] = int(value)
    return values.get("MemAvailable", values.get("MemFree", 0))


def swap_available_kib() -> int:
    values: dict[str, int] = {}
    with Path("/proc/meminfo").open() as handle:
        for line in handle:
            name, value, *_ = line.split()
            values[name.rstrip(":")] = int(value)
    return values.get("SwapFree", 0)


def load_one_minute() -> float:
    return os.getloadavg()[0]


def production_healthy(port: int) -> bool:
    request = urllib.request.Request(f"http://127.0.0.1:{port}/healthz", method="GET")
    try:
        with urllib.request.urlopen(request, timeout=2) as response:
            return response.status == 200
    except (OSError, urllib.error.URLError):
        return False


def resources_ok(args: argparse.Namespace) -> bool:
    memory = available_memory_kib()
    swap = swap_available_kib()
    load = load_one_minute()
    minimum = int(args.resume_memory_gib * 1024 * 1024)
    minimum_swap = int(args.resume_swap_gib * 1024 * 1024)
    production_ok = production_healthy(args.prod_port)
    if (
        memory < minimum
        or swap < minimum_swap
        or load >= args.resume_load
        or not production_ok
    ):
        log(
            "资源不足，等待编译: "
            f"MemAvailable={memory / 1024 / 1024:.2f}GiB "
            f"load1={load:.2f} "
            f"SwapFree={swap / 1024 / 1024:.2f}GiB "
            f"59138_health={'ok' if production_ok else 'failed'}"
        )
        return False
    return True


def wait_for_resources(args: argparse.Namespace) -> None:
    while not resources_ok(args):
        time.sleep(max(args.poll_seconds, 0.2))


def iter_watch_files() -> Iterable[Path]:
    for root_name in WATCH_ROOTS:
        root = REPO / root_name
        if root.is_file():
            yield root
            continue
        if not root.is_dir():
            continue
        for directory, dirnames, filenames in os.walk(root):
            dirnames[:] = sorted(name for name in dirnames if name not in IGNORE_DIRS)
            for name in sorted(filenames):
                path = Path(directory) / name
                if path.suffix in WATCH_SUFFIXES or name == "build.rs":
                    yield path


def source_snapshot() -> tuple[tuple[str, int, int], ...]:
    snapshot = []
    for path in iter_watch_files():
        try:
            stat = path.stat()
        except FileNotFoundError:
            continue
        snapshot.append((str(path.relative_to(REPO)), stat.st_mtime_ns, stat.st_size))
    return tuple(snapshot)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_line(pid: int) -> str:
    try:
        return Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace").strip()
    except (FileNotFoundError, PermissionError):
        return ""


def process_state(pid: int) -> str | None:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except (FileNotFoundError, PermissionError):
        return None
    closing = stat.rfind(")")
    if closing < 0 or closing + 2 >= len(stat):
        return None
    return stat[closing + 2]


def pid_is_ours(pid: int, args: argparse.Namespace) -> bool:
    line = command_line(pid)
    if not line:
        return False
    config = str(args.config)
    return config in line and ("kiro-rs" in line or str(args.binary) in line)


def read_pid(path: Path) -> int | None:
    try:
        value = path.read_text().strip()
        pid = int(value)
    except (FileNotFoundError, ValueError):
        return None
    return pid if pid > 1 else None


def find_existing_pid(args: argparse.Namespace, *, allow_discovery: bool) -> int | None:
    pid = read_pid(args.pid_file)
    if pid is not None and pid_is_ours(pid, args):
        return pid
    if not allow_discovery:
        return None
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        candidate = int(proc.name)
        if candidate != os.getpid() and pid_is_ours(candidate, args):
            return candidate
    return None


def port_is_open(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.2)
        return sock.connect_ex(("127.0.0.1", port)) == 0


def wait_for_port(port: int, expected: bool, timeout: float = 30.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if port_is_open(port) == expected:
            return True
        time.sleep(0.2)
    return False


def wait_for_health(port: int, timeout: float = 30.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if production_healthy(port):
            return True
        time.sleep(0.2)
    return False


def stop_server(args: argparse.Namespace) -> None:
    pid = find_existing_pid(args, allow_discovery=args.adopt_existing)
    if pid is None:
        if port_is_open(args.port):
            raise RuntimeError(
                f"端口 {args.port} 已占用，但 PID 未通过开发配置校验；拒绝停止未知进程"
            )
        return
    log(f"停止 59139 开发进程 pid={pid}")
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        state = process_state(pid)
        if state is None:
            break
        if state == "Z":
            try:
                os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                pass
            break
        time.sleep(0.2)
    else:
        log(f"开发进程未在 30 秒内退出，发送 SIGKILL pid={pid}")
        os.kill(pid, signal.SIGKILL)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and process_state(pid) is not None:
            try:
                os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                pass
            time.sleep(0.2)
    try:
        os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        pass
    if not wait_for_port(args.port, False, timeout=10):
        raise RuntimeError(f"59139 端口 {args.port} 未在进程退出后释放")
    args.pid_file.unlink(missing_ok=True)


def compile_binary(args: argparse.Namespace, log_handle) -> Path:
    wait_for_resources(args)
    args.target_dir.mkdir(parents=True, exist_ok=True)
    command = [
        "cargo",
        "build",
        "--bin",
        "kiro-rs",
        "-j",
        str(max(args.jobs, 1)),
    ]
    env = os.environ.copy()
    env.update(
        {
            "CARGO_TARGET_DIR": str(args.target_dir),
            "CARGO_INCREMENTAL": "1",
            "CARGO_PROFILE_DEV_DEBUG": "0",
            "CARGO_TERM_COLOR": "never",
        }
    )
    log(f"开始 debug 增量编译: {' '.join(command)} target={args.target_dir}")
    process = subprocess.Popen(
        command,
        cwd=REPO,
        env=env,
        stdout=log_handle,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    paused = False
    try:
        while process.poll() is None:
            memory = available_memory_kib()
            swap = swap_available_kib()
            load = load_one_minute()
            overloaded = (
                memory < int(args.pause_memory_gib * 1024 * 1024)
                or swap < int(args.pause_swap_gib * 1024 * 1024)
                or load >= args.pause_load
                or not production_healthy(args.prod_port)
            )
            if overloaded and not paused:
                log(
                    "编译资源超过阈值，暂停 cargo: "
                    f"MemAvailable={memory / 1024 / 1024:.2f}GiB "
                    f"SwapFree={swap / 1024 / 1024:.2f}GiB load1={load:.2f} "
                    f"59138_health={'ok' if production_healthy(args.prod_port) else 'failed'}"
                )
                os.killpg(process.pid, signal.SIGSTOP)
                paused = True
            elif paused and not overloaded and resources_ok(args):
                log("编译资源恢复，继续 cargo")
                os.killpg(process.pid, signal.SIGCONT)
                paused = False
            time.sleep(max(args.poll_seconds, 0.2))
    except KeyboardInterrupt:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        raise
    finally:
        if paused:
            os.killpg(process.pid, signal.SIGCONT)
    status = process.wait()
    if status != 0:
        raise RuntimeError(f"debug 增量编译失败，退出码 {status}；旧 59139 进程保持不变")
    built = args.target_dir / "debug" / "kiro-rs"
    if not built.is_file():
        raise RuntimeError(f"编译成功但找不到二进制: {built}")
    return built


def install_binary(built: Path, destination: Path) -> str:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(prefix=f".{destination.name}.", dir=destination.parent, delete=False) as handle:
        staged = Path(handle.name)
    try:
        staged.write_bytes(built.read_bytes())
        staged.chmod(0o755)
        os.replace(staged, destination)
    finally:
        staged.unlink(missing_ok=True)
    digest = sha256(destination)
    log(f"二进制已原子替换: {destination} sha256={digest}")
    return digest


def start_server(args: argparse.Namespace, digest: str, log_handle) -> int:
    command = [str(args.binary), "-c", str(args.config), "--credentials", str(args.credentials)]
    env = os.environ.copy()
    env["KIRO_RS_HOST"] = "0.0.0.0"
    process = subprocess.Popen(
        command,
        cwd=REPO,
        env=env,
        stdout=log_handle,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    args.pid_file.parent.mkdir(parents=True, exist_ok=True)
    args.pid_file.write_text(f"{process.pid}\n")
    if not wait_for_health(args.port):
        raise RuntimeError(f"59139 /healthz 未在 30 秒内就绪；pid={process.pid}，请查看 {args.log_file}")
    log(f"59139 已启动 pid={process.pid} binary={args.binary} sha256={digest}")
    return process.pid


def build_and_restart(args: argparse.Namespace, *, restart: bool) -> None:
    args.log_file.parent.mkdir(parents=True, exist_ok=True)
    with args.log_file.open("a", buffering=1) as log_handle:
        built = compile_binary(args, log_handle)
        prior_pid = (
            find_existing_pid(args, allow_discovery=args.adopt_existing)
            if restart
            else None
        )
        backup = None
        if prior_pid is not None:
            prior_executable = Path(f"/proc/{prior_pid}/exe")
            if not prior_executable.exists():
                raise RuntimeError(f"无法备份现有开发进程的二进制: pid={prior_pid}")
            with tempfile.NamedTemporaryFile(
                prefix=f".{args.binary.name}.previous.",
                dir=args.binary.parent,
                delete=False,
            ) as handle:
                backup = Path(handle.name)
            backup.write_bytes(prior_executable.read_bytes())
            backup.chmod(0o755)
        digest = install_binary(built, args.binary)
        if not restart:
            return
        try:
            stop_server(args)
            start_server(args, digest, log_handle)
        except Exception:
            if backup is None or not backup.exists():
                log("新开发二进制启动失败，且没有可用旧进程二进制回滚")
                raise
            log("新开发二进制未就绪，回滚到上一版并恢复 59139")
            failed_pid = find_existing_pid(args, allow_discovery=True)
            if failed_pid is not None:
                stop_server(args)
            os.replace(backup, args.binary)
            backup = None
            previous_digest = sha256(args.binary)
            start_server(args, previous_digest, log_handle)
            raise
        finally:
            if backup is not None:
                backup.unlink(missing_ok=True)


def run(args: argparse.Namespace) -> int:
    if args.jobs < 1 or args.poll_seconds <= 0:
        raise ValueError("--jobs 必须 >= 1，--poll-seconds 必须 > 0")
    if not (
        0 < args.pause_memory_gib <= args.resume_memory_gib
        and 0 < args.pause_swap_gib <= args.resume_swap_gib
        and 0 < args.resume_load <= args.pause_load
    ):
        raise ValueError("资源暂停阈值必须不高于恢复阈值；load 恢复阈值必须不高于暂停阈值")
    args.config = args.config.resolve()
    args.credentials = args.credentials.resolve()
    args.binary = args.binary.resolve()
    args.target_dir = args.target_dir.resolve()
    args.pid_file = args.pid_file.resolve()
    args.log_file = args.log_file.resolve()
    args.log_file.parent.mkdir(parents=True, exist_ok=True)
    if not args.config.is_file():
        raise FileNotFoundError(f"开发配置不存在: {args.config}")
    if not resources_ok(args):
        wait_for_resources(args)
    initial = source_snapshot()
    build_and_restart(args, restart=not args.check)
    if args.once or args.check:
        return 0
    log("开发热更新循环已启动；监听 Cargo.toml/Cargo.lock/build.rs/.cargo/src")
    try:
        while True:
            time.sleep(args.poll_seconds)
            current = source_snapshot()
            if current == initial:
                continue
            initial = current
            try:
                build_and_restart(args, restart=True)
            except Exception as error:  # keep the old process alive after a bad edit
                log(f"热更新失败，保留旧进程继续服务: {error}")
    except KeyboardInterrupt:
        log("收到 Ctrl-C，停止开发热更新循环；不自动停止 59139")
        return 130


if __name__ == "__main__":
    try:
        raise SystemExit(run(parse_args()))
    except Exception as error:
        log(str(error))
        raise SystemExit(1)
