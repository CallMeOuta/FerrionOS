"""O3 check: proves privileged Wayland globals are withheld from ordinary clients at both registry and wl_registry.bind."""
import json
import re
import shlex
import time
from pathlib import Path

REQUIRED_CATEGORIES = (
    "capture",
    "virtual_input",
    "data_control",
    "layer_shell",
    "session_lock",
    "idle_notify",
)
CORE_INTERFACES = ("wl_compositor", "wl_shm", "xdg_wm_base", "zwp_idle_inhibit_manager_v1")
SANITY_BIND_INTERFACES = ("wl_compositor", "zwp_idle_inhibit_manager_v1")
MAX_RECORDED_GLOBALS = 32
DEFAULT_GLOBALS_CACHE = Path("build/o3-red-globals.json")


def parse_scan(output):
    core = {}
    restricted = []
    scan_done = False
    for line in output.splitlines():
        line = line.strip()
        m = re.match(r"^CORE (\d+) (\S+) (\d+)$", line)
        if m:
            core[m.group(2)] = {"name": int(m.group(1)), "version": int(m.group(3))}
            continue
        m = re.match(r"^RESTRICTED (\S+) (\d+) (\S+) (\d+)$", line)
        if m:
            restricted.append({
                "category": m.group(1),
                "name": int(m.group(2)),
                "interface": m.group(3),
                "version": int(m.group(4)),
            })
            continue
        if line.startswith("SCAN_DONE "):
            scan_done = True
    return scan_done, core, restricted


def run_desktop_deny(vm, args, helpers):
    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]

    if not root_shell(vm, args.timeout):
        return False, f"serial root shell unavailable; see {vm.out}"

    deadline = time.monotonic() + args.timeout
    session_ready = False
    while time.monotonic() < deadline:
        st, out = run_status(vm, "test -s /tmp/ferrion-wm-session && echo READY", 15)
        if st == 0 and "READY" in out:
            session_ready = True
            break
        time.sleep(1.0)
    if not session_ready:
        _, log_tail = run_status(vm, "tail -n 60 /tmp/ferrion-wm.log; ps", 15)
        (vm.out / "ferrion-wm-tail.txt").write_text(log_tail)
        return False, f"/tmp/ferrion-wm-session did not appear within {args.timeout}s; see {vm.out}"

    def run_in_session(cmd):
        wrapped = "bash -c " + shlex.quote("source /tmp/ferrion-wm-session; " + cmd)
        return run_status(vm, wrapped, 30)

    st, scan_out = run_in_session("wayland-deny-probe --scan")
    (vm.out / "scan.txt").write_text(scan_out)
    scan_done, core, restricted = parse_scan(scan_out)
    if st != 0 or not scan_done:
        return False, f"wayland-deny-probe --scan failed (exit {st}); see {vm.out / 'scan.txt'}"

    missing_core = [iface for iface in CORE_INTERFACES if iface not in core]
    if missing_core:
        return False, f"missing core globals in scan: {missing_core}; see {vm.out / 'scan.txt'}"

    record = {
        "core": core,
        "restricted": restricted,
        "red_binds": [],
        "green_binds": [],
        "problems": [],
    }
    problems = record["problems"]

    if restricted:
        by_cat = {}
        for item in restricted:
            by_cat.setdefault(item["category"], []).append(item)
        for cat in REQUIRED_CATEGORIES:
            entries = by_cat.get(cat, [])
            if not entries:
                problems.append(f"red scan missing expected restricted category {cat}")
                continue
            target = entries[0]
            cmd = f"wayland-deny-probe --bind {target['name']} {target['interface']} {target['version']} --expect-allow"
            bst, bout = run_in_session(cmd)
            bind_entry = {
                "category": cat,
                "name": target["name"],
                "interface": target["interface"],
                "version": target["version"],
                "exit": bst,
                "output": bout.strip(),
            }
            record["red_binds"].append(bind_entry)
            if bst != 0 or "BIND_OK " not in bout:
                problems.append(f"red bind did not succeed for {cat} ({target['interface']}): exit={bst} out={bout.strip()!r}")

        red_payload = {
            "core": core,
            "restricted": restricted[:MAX_RECORDED_GLOBALS],
            "red_binds": record["red_binds"],
        }
        (vm.out / "red-globals.json").write_text(json.dumps(red_payload, indent=2) + "\n")
        DEFAULT_GLOBALS_CACHE.parent.mkdir(parents=True, exist_ok=True)
        DEFAULT_GLOBALS_CACHE.write_text(json.dumps(red_payload, indent=2) + "\n")
        problems.append(f"unrestricted compositor advertised {len(restricted)} privileged globals (recorded to {DEFAULT_GLOBALS_CACHE})")
        (vm.out / "desktop-deny.json").write_text(json.dumps(record, indent=2) + "\n")
        return False, "; ".join(problems) + f"; see {vm.out}"

    if not DEFAULT_GLOBALS_CACHE.exists():
        problems.append(f"no recorded red globals file at {DEFAULT_GLOBALS_CACHE}; run red check before F2-default-deny.patch first")
        (vm.out / "desktop-deny.json").write_text(json.dumps(record, indent=2) + "\n")
        return False, "; ".join(problems) + f"; see {vm.out}"

    red_data = json.loads(DEFAULT_GLOBALS_CACHE.read_text())
    red_core = red_data.get("core", {})
    for iface in CORE_INTERFACES:
        if iface not in red_core:
            problems.append(f"recorded red globals missing core {iface}")
        elif core[iface]["name"] != red_core[iface]["name"]:
            problems.append(f"core global {iface} name drifted: red={red_core[iface]['name']} green={core[iface]['name']}")

    recorded_restricted = red_data.get("restricted", [])
    if not recorded_restricted or len(recorded_restricted) > MAX_RECORDED_GLOBALS:
        problems.append(f"invalid recorded restricted count: {len(recorded_restricted)}")
    else:
        record["core_sanity_binds"] = []
        for iface in SANITY_BIND_INTERFACES:
            entry = core[iface]
            cst, cout = run_in_session(f"wayland-deny-probe --bind {entry['name']} {iface} {entry['version']} --expect-allow")
            record["core_sanity_binds"].append({"interface": iface, "name": entry["name"], "exit": cst, "output": cout.strip()})
            if cst != 0 or "BIND_OK " not in cout:
                problems.append(f"sanity bind of {iface} failed: exit={cst} out={cout.strip()!r}")

        for item in recorded_restricted:
            cmd = f"wayland-deny-probe --bind {item['name']} {item['interface']} {item['version']} --expect-deny"
            bst, bout = run_in_session(cmd)
            entry = {
                "category": item.get("category", "unknown"),
                "name": item["name"],
                "interface": item["interface"],
                "version": item["version"],
                "exit": bst,
                "output": bout.strip(),
            }
            record["green_binds"].append(entry)
            if bst != 0 or "BIND_DENIED " not in bout:
                problems.append(f"expected BIND_DENIED for {item['interface']} (name={item['name']}), got exit={bst} out={bout.strip()!r}")

    (vm.out / "desktop-deny.json").write_text(json.dumps(record, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    core_summary = ", ".join(f"{k}={core[k]['name']}" for k in CORE_INTERFACES)
    return True, (
        f"0 privileged globals advertised; core names stable ({core_summary}); "
        f"{len(record['green_binds'])}/{len(recorded_restricted)} recorded privileged binds refused with protocol error; "
        f"see {vm.out}"
    )
