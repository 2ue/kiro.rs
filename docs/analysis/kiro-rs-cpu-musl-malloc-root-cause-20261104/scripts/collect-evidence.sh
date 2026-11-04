#!/usr/bin/env bash
# 采集 kiro-rs 容器 CPU 根因证据，只读，不改服务。需要宿主机 root 和 linux-perf。
# 用法: collect-evidence.sh <容器名> <输出目录> [逐秒采样秒数，默认 120]
# 输出日志会去掉 ANSI 颜色并把邮箱替换为 <email>。
set -euo pipefail
C=$1; OUT=$2; SECS=${3:-120}
mkdir -p "$OUT"; cd "$OUT"
PID=$(docker inspect -f '{{.State.Pid}}' "$C")
mask() { sed -u -E 's/\x1b\[[0-9;]*m//g; s/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/<email>/g'; }

# 00 环境：机器、镜像、二进制指纹、进程内存、所有容器 CPU
{
  echo "collected_at=$(date -Is)"; echo "host=$(hostname)"; echo "kernel=$(uname -r)"; echo "nproc=$(nproc)"
  free -m; uptime
  docker inspect -f 'container={{.Name}} image={{.Config.Image}} image_id={{.Image}} version={{index .Config.Labels "org.opencontainers.image.version"}} nano_cpus={{.HostConfig.NanoCpus}} mem_limit={{.HostConfig.Memory}} started={{.State.StartedAt}}' "$C"
  echo "pid=$PID binary_sha256=$(sha256sum /proc/$PID/exe | cut -d' ' -f1)"
  grep -E 'Threads|VmRSS|VmHWM' /proc/$PID/status
  echo "--- docker stats"; docker stats --no-stream --format '{{.Name}} {{.CPUPerc}} {{.MemUsage}}'
} > 00-environment.txt 2>&1

# 10/11 应用日志 + 逐秒 CPU（单位：USER_HZ=100 的 tick，100 tick/s = 1 个核）
T_START=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "epoch utime_ticks stime_ticks minor_faults rss_mb" > 11-per-second-cpu.txt
read -r u0 s0 f0 < <(awk '{print $14,$15,$10}' /proc/$PID/stat)
for _ in $(seq 1 "$SECS"); do
  sleep 1
  read -r u s f < <(awk '{print $14,$15,$10}' /proc/$PID/stat)
  echo "$(date +%s) $((u-u0)) $((s-s0)) $((f-f0)) $(awk '/VmRSS/{print int($2/1024)}' /proc/$PID/status)" >> 11-per-second-cpu.txt
  u0=$u; s0=$s; f0=$f
done
# 采样窗口结束后一次性导出这段时间的日志，不用 -f 跟随，避免残留进程
docker logs --since "$T_START" --until "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$C" 2>&1 | mask > 10-app.log

# 20 按线程统计 10 秒 user/sys
snap() { for t in /proc/$PID/task/*; do echo "$(basename "$t") $(cat "$t/comm" | tr ' ' _) $(awk '{print $14,$15}' "$t/stat")"; done | sort; }
snap > .t0; sleep 10; snap > .t1
join .t0 .t1 | awk '{du=$6-$3; ds=$7-$4; tu+=du; ts+=ds; printf "%s %s user=%d%% sys=%d%%\n",$1,$2,du/10,ds/10} END{printf "TOTAL user=%d%% sys=%d%% (10s)\n",tu/10,ts/10}' > 20-threads-10s.txt
rm -f .t0 .t1

# 30 系统调用 / 缺页 / TLB 计数，10 秒
perf stat -p "$PID" -e 'syscalls:sys_enter_futex,syscalls:sys_enter_madvise,syscalls:sys_enter_mmap,syscalls:sys_enter_munmap,syscalls:sys_enter_mremap,syscalls:sys_enter_sendto,syscalls:sys_enter_write,syscalls:sys_enter_read,page-faults,context-switches,tlb:tlb_flush' -- sleep 10 > 30-perf-stat-10s.txt 2>&1

# 40 CPU 采样 30 秒（199Hz，带调用栈）
perf record -F 199 -g -p "$PID" -o 40-perf-cpu.data -- sleep 30 > /dev/null 2>&1
perf report -i 40-perf-cpu.data --no-children --sort dso,sym --stdio -g none --percent-limit 0 -F overhead,sample,dso,sym > 41-perf-cpu-flat.txt 2>/dev/null
perf report -i 40-perf-cpu.data --no-children --sort dso,sym --stdio -g caller,2,callee --percent-limit 1 > 42-perf-cpu-callgraph.txt 2>/dev/null

# 50 futex 调用点归因 10 秒（每 20000 次取 1 个样本，控制文件大小）
perf record -e syscalls:sys_enter_futex -c 20000 -p "$PID" -o 50-perf-futex.data -- sleep 10 > /dev/null 2>&1
perf report -i 50-perf-futex.data --no-children --sort dso,sym --stdio -g none --percent-limit 0 -F overhead,sample,dso,sym > 51-perf-futex-flat.txt 2>/dev/null
rm -f 50-perf-futex.data
gzip -f 40-perf-cpu.data
echo "done $(date -Is)" >> 00-environment.txt
