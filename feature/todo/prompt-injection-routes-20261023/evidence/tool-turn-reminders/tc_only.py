import importlib.util, json, subprocess, sys, time
sys.argv = ["x", "claude-haiku-4-5", "6"]
spec = importlib.util.spec_from_file_location("m", "/tmp/acct-e2e/reminder_effect.py"); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
report = {}
for label, binary in m.BINARIES.items():
    s = subprocess.run([f"{m.ROOT}/start.sh", binary], capture_output=True, text=True, timeout=200); assert "started" in s.stdout
    time.sleep(3)
    m.set_items({"toolChoicePrefix": True, "toolChoiceTurnReminder": True, "languageTurnReminder": True, "taskQuality": False})
    report[label] = m.tool_choice_run(label)
    json.dump(report, open(f"{m.ROOT}/tc-haiku.json", "w"), ensure_ascii=False, indent=2)
for label, rows in report.items():
    for choice in ("any", "tool"):
        part = [r for r in rows if r["choice"] == choice and r["status"] == 200]
        print(f"== {label} {choice}: ok={len(part)} complied={sum(r['complied'] for r in part)}")
