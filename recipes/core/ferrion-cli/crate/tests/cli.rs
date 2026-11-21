use std::process::{Command, Stdio};

#[test]
fn json_stdout_and_process_exit_agree() {
    for (args, code, command) in [
        (vec!["help", "--json"], 0, "help"),
        (vec!["--json", "help", "--yes"], 0, "help"),
        (vec!["install", "--json"], 2, "invalid"),
        (vec!["theme", "--json"], 2, "invalid"),
        (vec!["remove", "--all", "--json"], 2, "invalid"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ferrion"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stderr.is_empty());
        let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(record["exit_code"], code);
        assert_eq!(record["ok"], code == 0);
        assert_eq!(record["command"], command);
        if command == "help" {
            assert!(record["data"]["help"].as_str().unwrap().contains("--yes"));
        }
    }
}
