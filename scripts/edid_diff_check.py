#!/usr/bin/env python3
import argparse
import hashlib
import json
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

from edid_fixtures import (
    MAX_CORPUS_FIXTURES,
    MAX_FIXTURE_BYTES,
    SKIP_DMT_BLOCKS,
    get_edid_decode_version,
    parse_edid_decode_output,
    run_edid_decode,
)

STEP3_ADAPTER_RS = r"""
#[path = "__EDID_RS__"]
mod edid;

use std::env;
use std::fs;

fn err_name(err: edid::EdidError) -> &'static str {
    match err {
        edid::EdidError::InvalidLength => "InvalidLength",
        edid::EdidError::BadHeader => "BadHeader",
        edid::EdidError::BadBaseChecksum => "BadBaseChecksum",
        edid::EdidError::NoTiming => "NoTiming",
    }
}

fn main() {
    let path = env::args().nth(1).expect("missing edid path");
    let bytes = fs::read(&path).expect("failed to read edid");
    match edid::decode_preferred(&bytes) {
        Ok(dec) => {
            println!(
                "{{\"modes\":[{{\"width\":{},\"height\":{},\"refresh_mhz\":null,\"preferred\":true,\"source_block\":null}}],\"preferred\":{{\"width\":{},\"height\":{},\"refresh_mhz\":null,\"preferred\":true,\"source_block\":null}},\"error\":null,\"extensions_truncated\":{},\"bad_extension_blocks\":{},\"modes_truncated\":false}}",
                dec.preferred.width,
                dec.preferred.height,
                dec.preferred.width,
                dec.preferred.height,
                dec.extensions_truncated,
                dec.bad_extension_blocks
            );
        }
        Err(err) => {
            println!(
                "{{\"modes\":[],\"preferred\":null,\"error\":\"{}\",\"extensions_truncated\":false,\"bad_extension_blocks\":0,\"modes_truncated\":false}}",
                err_name(err)
            );
        }
    }
}
"""

R1A_ADAPTER_RS = r"""
#[path = "__EDID_RS__"]
mod edid;

use std::env;
use std::fs;

fn err_name(err: edid::EdidError) -> &'static str {
    match err {
        edid::EdidError::InvalidLength => "InvalidLength",
        edid::EdidError::BadHeader => "BadHeader",
        edid::EdidError::BadBaseChecksum => "BadBaseChecksum",
        edid::EdidError::NoTiming => "NoTiming",
    }
}

fn fmt_mode(m: &edid::ModeTiming) -> String {
    format!(
        "{{\"width\":{},\"height\":{},\"refresh_mhz\":{},\"preferred\":{},\"source_block\":\"{}\"}}",
        m.width,
        m.height,
        m.refresh_mhz,
        m.preferred,
        m.source.label()
    )
}

fn main() {
    let path = env::args().nth(1).expect("missing edid path");
    let bytes = fs::read(&path).expect("failed to read edid");
    match edid::decode_all(&bytes) {
        Ok(dec) => {
            let modes_json: Vec<String> = dec.modes[..dec.mode_count]
                .iter()
                .map(fmt_mode)
                .collect();
            let pref_json = fmt_mode(&dec.preferred);
            println!(
                "{{\"modes\":[{}],\"preferred\":{},\"error\":null,\"extensions_truncated\":{},\"bad_extension_blocks\":{},\"modes_truncated\":{}}}",
                modes_json.join(","),
                pref_json,
                dec.extensions_truncated,
                dec.bad_extension_blocks,
                dec.modes_truncated
            );
        }
        Err(err) => {
            println!(
                "{{\"modes\":[],\"preferred\":null,\"error\":\"{}\",\"extensions_truncated\":false,\"bad_extension_blocks\":0,\"modes_truncated\":false}}",
                err_name(err)
            );
        }
    }
}
"""


def build_harness_in_container(
    container_cmd: list[str],
    edid_rs_rel: str,
    mode: str,
    work_dir: Path,
    repo_root: Path,
) -> Path:
    src_text = (repo_root / edid_rs_rel).read_text(encoding="utf-8")
    if mode == "auto":
        mode = "r1a" if "pub fn decode_all(" in src_text else "step3"
    assert mode in ("step3", "r1a")

    rel_work = work_dir.relative_to(repo_root)
    container_edid_rs = f"/mnt/redox/{edid_rs_rel}"
    adapter_src = (STEP3_ADAPTER_RS if mode == "step3" else R1A_ADAPTER_RS).replace(
        "__EDID_RS__", container_edid_rs
    )
    adapter_path = work_dir / f"adapter_{mode}.rs"
    adapter_path.write_text(adapter_src, encoding="utf-8")
    bin_path = work_dir / f"edid_harness_{mode}"

    cmd = container_cmd + [
        "rustc",
        "--edition",
        "2021",
        "-O",
        f"/mnt/redox/{rel_work / adapter_path.name}",
        "-o",
        f"/mnt/redox/{rel_work / bin_path.name}",
    ]
    subprocess.run(cmd, check=True)
    return bin_path


def run_harness(container_cmd: list[str], bin_path: Path, edid_path: Path, repo_root: Path) -> dict:
    rel_bin = bin_path.relative_to(repo_root)
    rel_edid = edid_path.relative_to(repo_root)
    cmd = container_cmd + [f"/mnt/redox/{rel_bin}", f"/mnt/redox/{rel_edid}"]
    out = subprocess.check_output(cmd).decode("utf-8", errors="replace").strip()
    return json.loads(out)


def mode_key_size_only(m: dict) -> tuple:
    return (m["width"], m["height"])


def compare_fixture(
    fixture: dict,
    edid_bytes: bytes,
    oracle_cmd: list[str],
    actual: dict,
) -> dict:
    malformed = fixture.get("malformed")
    pref_div = fixture.get("preferred_divergence")

    if malformed and malformed["expected_error"] is not None:
        exp_err = malformed["expected_error"]
        diffs = {}
        if actual["error"] != exp_err:
            diffs["error"] = {"expected": exp_err, "actual": actual["error"]}
        if actual["modes"]:
            diffs["modes"] = {"expected": [], "actual": actual["modes"]}
        if actual["preferred"] is not None:
            diffs["preferred"] = {"expected": None, "actual": actual["preferred"]}
        return {"name": fixture["name"], "ok": len(diffs) == 0, "diffs": diffs}

    raw_oracle, _rc = run_edid_decode(oracle_cmd, edid_bytes)
    oracle = parse_edid_decode_output(raw_oracle)

    if malformed and malformed.get("expected_extensions_truncated"):
        oracle_modes = [m for m in oracle["modes"] if m["source_block"] == "base-dtd"]
    else:
        oracle_modes = oracle["modes"]

    diffs = {}
    if actual["error"] is not None:
        diffs["error"] = {"expected": None, "actual": actual["error"]}

    exp_trunc = bool(malformed and malformed.get("expected_extensions_truncated"))
    if bool(actual.get("extensions_truncated")) != exp_trunc:
        diffs["extensions_truncated"] = {
            "expected": exp_trunc,
            "actual": bool(actual.get("extensions_truncated")),
        }

    act_modes = actual["modes"]
    if len(act_modes) != len(oracle_modes):
        diffs["modes"] = {"expected_count": len(oracle_modes), "actual_count": len(act_modes)}
    else:
        size_mismatches = []
        refresh_mismatches = []
        source_mismatches = []
        for idx, (exp_m, act_m) in enumerate(zip(oracle_modes, act_modes)):
            if mode_key_size_only(exp_m) != mode_key_size_only(act_m):
                size_mismatches.append((idx, exp_m, act_m))
            if act_m["refresh_mhz"] is None or abs(act_m["refresh_mhz"] - exp_m["refresh_mhz"]) > 1:
                refresh_mismatches.append((idx, exp_m["refresh_mhz"], act_m["refresh_mhz"]))
            if act_m["source_block"] != exp_m["source_block"]:
                source_mismatches.append((idx, exp_m["source_block"], act_m["source_block"]))
        if size_mismatches:
            diffs["modes"] = size_mismatches
        if refresh_mismatches:
            diffs["refresh_mhz"] = refresh_mismatches
        if source_mismatches:
            diffs["source_block"] = source_mismatches

    oracle_pref = oracle["edid_decode_preferred"]
    act_pref = actual["preferred"]
    if pref_div is not None:
        exp_pref = pref_div["ferrion_preferred"]
        decl_oracle_pref = pref_div["edid_decode_preferred"]
        if decl_oracle_pref != oracle_pref:
            diffs["preferred"] = {
                "reason": "manifest edid_decode_preferred mismatch",
                "manifest": decl_oracle_pref,
                "oracle": oracle_pref,
            }
    else:
        exp_pref = oracle_pref

    if "preferred" not in diffs:
        if exp_pref is None and act_pref is None:
            pass
        elif exp_pref is None or act_pref is None:
            diffs["preferred"] = {"expected": exp_pref, "actual": act_pref}
        else:
            if (exp_pref["width"], exp_pref["height"]) != (act_pref["width"], act_pref["height"]):
                diffs["preferred"] = {"expected": exp_pref, "actual": act_pref}
            if act_pref["refresh_mhz"] is None or abs(act_pref["refresh_mhz"] - exp_pref["refresh_mhz"]) > 1:
                diffs.setdefault("refresh_mhz", []).append(
                    ("preferred", exp_pref["refresh_mhz"], act_pref["refresh_mhz"])
                )
            if act_pref["source_block"] != exp_pref["source_block"]:
                diffs.setdefault("source_block", []).append(
                    ("preferred", exp_pref["source_block"], act_pref["source_block"])
                )

    return {"name": fixture["name"], "ok": len(diffs) == 0, "diffs": diffs}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Differential EDID check against container edid-decode 1.32.0.")
    parser.add_argument("--corpus-dir", type=Path, default=Path("scripts/edid-corpus"))
    parser.add_argument(
        "--edid-rs",
        required=True,
        help="Path relative to repo root of fetched bootloader src/edid.rs",
    )
    parser.add_argument(
        "--mode",
        choices=("auto", "step3", "r1a"),
        default="auto",
        help="Bootloader edid.rs adapter API",
    )
    parser.add_argument(
        "--container-cmd",
        required=True,
        help="Prefix command to execute tools inside the build container with repo mounted at /mnt/redox",
    )
    parser.add_argument("--out", type=Path, default=None, help="Optional JSON report path")
    args = parser.parse_args(argv)

    repo_root = Path.cwd().resolve()
    container_cmd = shlex.split(args.container_cmd)
    oracle_cmd = container_cmd + ["edid-decode"]
    oracle_version = get_edid_decode_version(oracle_cmd)

    manifest_path = args.corpus_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    fixtures = manifest["fixtures"]
    assert len(fixtures) <= MAX_CORPUS_FIXTURES, f"{len(fixtures)} > {MAX_CORPUS_FIXTURES}"

    build_tmp_base = repo_root / "build" / "edid-diff-tmp"
    build_tmp_base.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=build_tmp_base, prefix="run-") as tmpdir:
        work_dir = Path(tmpdir)
        bin_path = build_harness_in_container(container_cmd, args.edid_rs, args.mode, work_dir, repo_root)

        field_names = ("modes", "preferred", "refresh_mhz", "source_block", "error", "extensions_truncated")
        field_counts = {k: 0 for k in field_names}
        results = []
        differing = 0

        for fixture in fixtures:
            edid_path = args.corpus_dir / fixture["file"]
            raw = edid_path.read_bytes()
            assert 1 <= len(raw) <= MAX_FIXTURE_BYTES
            assert hashlib.sha256(raw).hexdigest() == fixture["sha256"], f"sha256 mismatch for {edid_path}"

            actual = run_harness(container_cmd, bin_path, edid_path.resolve(), repo_root)
            res = compare_fixture(fixture, raw, oracle_cmd, actual)
            results.append(res)
            if not res["ok"]:
                differing += 1
                for field in field_names:
                    if field in res["diffs"]:
                        field_counts[field] += 1

    divergences = [
        {"name": f["name"], "reason": f["preferred_divergence"]["reason"]}
        for f in fixtures
        if f.get("preferred_divergence") is not None
    ]
    report = {
        "oracle_version": oracle_version,
        "skip_dmt_blocks": list(SKIP_DMT_BLOCKS),
        "edid_rs": args.edid_rs,
        "mode": args.mode,
        "total_fixtures": len(fixtures),
        "differing_fixtures": differing,
        "field_diff_counts": field_counts,
        "deliberate_divergences": divergences,
        "results": results,
    }
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    print(f"oracle: {oracle_version}")
    print(f"skip_dmt_blocks: {', '.join(SKIP_DMT_BLOCKS)}")
    print(f"fixtures: {differing}/{len(fixtures)} differ")
    print(
        "per-field diffs: "
        + ", ".join(f"{k}={field_counts[k]}" for k in field_names)
    )
    print(f"deliberate_divergences ({len(divergences)}):")
    for div in divergences:
        print(f"  - {div['name']}: {div['reason']}")
    for res in results:
        if not res["ok"]:
            print(f"  DIFF {res['name']}: {', '.join(sorted(res['diffs'].keys()))}")
    return 0 if differing == 0 else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
