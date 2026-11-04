#!/usr/bin/env python3
"""把 collect-evidence.sh 采集的逐秒 CPU 和应用日志对齐，验证 CPU 与请求量 / 字节 / 重试的相关性。

用法: analyze-load.py <证据目录>
读取 10-app.log 和 11-per-second-cpu.txt，只读。
"""
import re, sys, datetime, collections, statistics as st

d = sys.argv[1]
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)\.\d+Z")
rows = []
for l in list(open(f"{d}/11-per-second-cpu.txt"))[1:]:
    ts, u, s, f, rss = map(int, l.split())
    rows.append(dict(t=ts - 1, u=u, s=s, cpu=u + s, flt=f, rss=rss))

T0, T1 = rows[0]["t"] - 1, rows[-1]["t"]
send_b = collections.Counter(); sends = collections.Counter(); retries = collections.Counter()
first = 0; reasons = collections.Counter(); retried = set(); reqs = set(); sizes = []
for l in open(f"{d}/10-app.log", errors="replace"):
    m = TS.match(l)
    if not m: continue
    t = int(datetime.datetime.fromisoformat(m.group(1)).replace(tzinfo=datetime.timezone.utc).timestamp())
    if not T0 <= t <= T1: continue  # 只统计 CPU 采样窗口内的日志
    if "Kiro upstream request body size" in l:
        b = int(re.search(r"upstream_body_bytes=(\d+)", l).group(1))
        send_b[t] += b; sends[t] += 1; sizes.append(b)
        r = re.search(r'request_id="?([^" ]+)', l); reqs.add(r and r.group(1))
        if re.search(r"\battempt=1\b", l): first += 1
    if "准备换号重试" in l:
        retries[t] += 1
        r = re.search(r'reason="?([^" ]+)', l); reasons[r and r.group(1)] += 1
        r = re.search(r"request_id=([^ ]+)", l); retried.add(r and r.group(1))

def corr(a, b):
    ma, mb = st.mean(a), st.mean(b)
    n = sum((x - ma) * (y - mb) for x, y in zip(a, b))
    den = (sum((x - ma) ** 2 for x in a) * sum((y - mb) ** 2 for y in b)) ** .5
    return round(n / den, 2) if den else float("nan")

cpu = [r["cpu"] for r in rows]
print(f"采样秒数 {len(rows)}（窗口 {datetime.datetime.fromtimestamp(T0, datetime.timezone.utc):%Y-%m-%d %H:%M:%S} - {datetime.datetime.fromtimestamp(T1, datetime.timezone.utc):%H:%M:%S} UTC）")
print(f"kiro-rs CPU（100%=1 核）: 平均 {st.mean(cpu):.0f}%  中位 {st.median(cpu):.0f}%  最大 {max(cpu)}%  最小 {min(cpu)}%")
print(f"  其中 user 平均 {st.mean(r['u'] for r in rows):.0f}%  sys 平均 {st.mean(r['s'] for r in rows):.0f}%")
print(f"RSS: {min(r['rss'] for r in rows)}-{max(r['rss'] for r in rows)} MB")
print(f"上游发送 {sum(sends.values())} 次（首发 {first}），不同请求 {len(reqs)} 个，换号重试 {sum(retries.values())} 次，涉及 {len(retried)} 个请求，原因 {dict(reasons)}")
if sizes:
    ss = sorted(sizes)
    print(f"发送体积: 合计 {sum(ss)/1e6:.0f} MB，中位 {ss[len(ss)//2]/1e3:.0f} KB，P90 {ss[int(len(ss)*.9)]/1e6:.2f} MB，最大 {ss[-1]/1e6:.1f} MB")
print("\n逐秒相关系数（Pearson，CPU 对齐到同一秒的日志事件）:")
for name, xs in [("发送字节(当秒+前一秒)", [send_b[r['t']] + send_b[r['t'] - 1] for r in rows]),
                 ("发送次数", [sends[r['t']] for r in rows]),
                 ("重试次数", [retries[r['t']] for r in rows]),
                 ("缺页次数", [r['flt'] for r in rows])]:
    print(f"  CPU ~ {name}: {corr(cpu, xs)}")
print("\nCPU 最高的 10 秒: epoch cpu% (user/sys) 当秒发送MB 发送次数 重试 缺页")
for r in sorted(rows, key=lambda r: -r["cpu"])[:10]:
    print(f"  {r['t']} {r['cpu']}% ({r['u']}/{r['s']}) {send_b[r['t']]/1e6:.1f} {sends[r['t']]} {retries[r['t']]} {r['flt']}")
