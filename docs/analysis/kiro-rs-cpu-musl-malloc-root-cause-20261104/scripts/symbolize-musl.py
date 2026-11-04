#!/usr/bin/env python3
"""把 strip 后的 kiro-rs 二进制里的热点地址映射到 musl 函数名。

原理：Rust 的 x86_64-unknown-linux-musl 目标静态链接工具链自带的
self-contained/libc.a。把其中每个函数的机器码（重定位字段置为通配）在生产
二进制的 .text 里搜索，唯一命中即得到该函数在二进制中的地址范围。

用法: symbolize-musl.py <kiro-rs 二进制> <libc.a> <perf flat 报告>... > 输出
依赖: binutils (objdump/readelf/ar)。只读，不执行任何被分析的文件。
"""
import re, subprocess, sys, tempfile, os, collections

def run(*a, **kw):
    return subprocess.run(a, check=True, capture_output=True, text=True, **kw).stdout

def text_section(binary):
    # 返回 (.text 文件偏移, 虚拟地址, 字节)
    for l in run("readelf", "-SW", binary).splitlines():
        m = re.search(r"\]\s+\.text\s+PROGBITS\s+([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)", l)
        if m:
            addr, off, size = (int(x, 16) for x in m.groups())
            with open(binary, "rb") as f:
                f.seek(off); return addr, f.read(size)
    sys.exit("no .text")

def functions_in_archive(libc):
    """解析 libc.a 里所有函数：名字 -> (字节, 需要通配的偏移集合)。"""
    tmp = tempfile.mkdtemp()
    run("ar", "x", os.path.abspath(libc), cwd=tmp)
    funcs = {}
    for obj in sorted(os.listdir(tmp)):
        p = os.path.join(tmp, obj)
        dis = run("objdump", "-d", "-r", "-z", p)
        cur = None; buf = bytearray(); wild = set(); start = 0
        def flush():
            if cur and len(buf) >= 24:
                funcs.setdefault(cur, (bytes(buf), frozenset(wild), obj))
        for l in dis.splitlines():
            m = re.match(r"^([0-9a-f]+) <([^>]+)>:", l)
            if m:
                flush(); cur = m.group(2); buf = bytearray(); wild = set(); start = int(m.group(1), 16); continue
            m = re.match(r"^\s+([0-9a-f]+):\s+((?:[0-9a-f]{2} )+)", l)
            if m and cur:
                off = int(m.group(1), 16) - start
                bs = bytes.fromhex(m.group(2).replace(" ", ""))
                if off == len(buf): buf += bs
                continue
            m = re.match(r"^\s+([0-9a-f]+):\s+R_X86_64_(\w+)", l)
            if m and cur:
                off = int(m.group(1), 16) - start
                width = 8 if m.group(2) == "64" else 4
                wild.update(range(off, off + width))
        flush()
    return funcs

def find(text, pat, wild):
    # 先用一段不含通配的锚点快速定位，再整段校验
    best = None
    for i in range(0, len(pat) - 8):
        if not any(j in wild for j in range(i, i + 8)):
            best = i; break
    if best is None: return []
    anchor = pat[best:best + 8]; hits = []; pos = text.find(anchor)
    while pos != -1 and len(hits) < 3:
        s = pos - best
        if s >= 0 and all(j in wild or text[s + j] == pat[j] for j in range(len(pat))):
            hits.append(s)
        pos = text.find(anchor, pos + 1)
    return hits

def main():
    binary, libc, reports = sys.argv[1], sys.argv[2], sys.argv[3:]
    base, text = text_section(binary)
    funcs = functions_in_archive(libc)
    ranges = []
    for name, (pat, wild, obj) in funcs.items():
        hits = find(text, pat, wild)
        if len(hits) == 1:
            ranges.append((base + hits[0], base + hits[0] + len(pat), name, obj))
    ranges.sort()
    print(f"# libc.a 函数 {len(funcs)} 个，在二进制中唯一定位 {len(ranges)} 个")
    lo, hi = ranges[0][0], ranges[-1][1]
    print(f"# 已定位 musl 函数地址范围: {lo:#x} - {hi:#x}")
    for want in ("__lock", "__unlock", "__libc_malloc_impl", "free", "__libc_realloc", "__syscall_cp_c", "memcpy", "memmove"):
        r = [x for x in ranges if x[2] == want]
        print(f"#   {want:22s} " + (f"{r[0][0]:#x}-{r[0][1]:#x} ({r[0][3]})" if r else "未唯一定位"))
    def sym(ip):
        for a, b, n, o in ranges:
            if a <= ip < b: return n
        return None
    for rep in reports:
        print(f"\n## {rep}")
        per = collections.Counter(); cat = collections.Counter(); total = 0.0
        for l in open(rep):
            m = re.match(r"\s+([0-9.]+)%\s+\d+\s+(\S+)\s+\[(.)\]\s+(\S+)", l) or re.match(r"\s+([0-9.]+)%\s+(\S+)\s+\[(.)\]\s+(\S+)", l)
            if not m: continue
            p = float(m.group(1)); dso = m.group(2); s = m.group(4); total += p
            if dso.startswith("[kernel"):
                cat["kernel"] += p; continue
            try: ip = int(s, 16)
            except ValueError:
                cat["其他(" + s + ")"] += p; continue
            n = sym(ip)
            if n: cat["musl libc"] += p; per[n] += p
            else: cat["应用 + Rust 依赖库"] += p
        print(f"合计 {total:.1f}%")
        for k, v in cat.most_common(): print(f"  {v:5.1f}%  {k}")
        print("  musl 函数明细:")
        for k, v in per.most_common(15): print(f"    {v:5.1f}%  {k}")

main()
