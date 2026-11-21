use std::fs;
use std::io;
use std::process::ExitCode;

use libredox::protocol::NsDup;
use libredox::{call, errno};

fn fork_request(len: usize) -> Vec<u8> {
    let mut request = (NsDup::ForkNs as usize).to_le_bytes().to_vec();
    request.extend_from_slice(&len.to_le_bytes());
    request.extend_from_slice(b"file");
    request
}

fn describe(errno: i32) -> String {
    format!("errno {errno} ({})", io::Error::from_raw_os_error(errno))
}

fn main() -> ExitCode {
    let mut failed = false;
    let mut report = |check: &str, ok: bool, detail: String| {
        let verdict = if ok { "pass" } else { "FAIL" };
        println!("nsmgr-bounds-probe: {verdict} {check}: {detail}");
        failed |= !ok;
    };

    let ns = match call::getns() {
        Ok(ns) => ns,
        Err(err) => {
            report(
                "namespace",
                false,
                format!("no namespace handle: {}", describe(err.errno())),
            );
            return ExitCode::FAILURE;
        }
    };

    for (check, len) in [("past-end", 4096), ("overflow", usize::MAX)] {
        match call::dup(ns, fork_request(len)) {
            Ok(fd) => {
                let _ = call::close(fd);
                report(check, false, format!("a name of {len} bytes was accepted"));
            }
            Err(err) if err.errno() == errno::EINVAL => {
                report(
                    check,
                    true,
                    format!("a name of {len} bytes: {}", describe(err.errno())),
                );
            }
            Err(err) => report(
                check,
                false,
                format!("a name of {len} bytes: {}", describe(err.errno())),
            ),
        }
    }

    match call::dup(ns, fork_request(4)) {
        Ok(fd) => {
            let _ = call::close(fd);
            report("well-formed", true, "a fork naming `file` works".to_owned());
        }
        Err(err) => report(
            "well-formed",
            false,
            format!("a fork naming `file`: {}", describe(err.errno())),
        ),
    }
    match fs::read_dir("/scheme") {
        Ok(entries) => {
            let count = entries.filter(Result::is_ok).count();
            report(
                "still-answers",
                count > 0,
                format!("/scheme lists {count} schemes"),
            );
        }
        Err(err) => report("still-answers", false, format!("/scheme: {err}")),
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
