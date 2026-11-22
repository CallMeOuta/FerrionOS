use std::process;

use ferrion_run::{
    Command, EXIT_REFUSED, USAGE, parse_args, parse_profile, profile_path, sessions,
};

fn refuse(reason: &str) -> ! {
    eprintln!("ferrion-run: {reason}");
    process::exit(EXIT_REFUSED);
}

fn done(result: Result<(), String>) -> ! {
    match result {
        Ok(()) => process::exit(0),
        Err(reason) => {
            eprintln!("ferrion-run: {reason}");
            process::exit(1);
        }
    }
}

fn main() {
    let options = match parse_args(std::env::args_os().skip(1)) {
        Ok(Command::Help) => {
            print!("{USAGE}");
            return;
        }
        Ok(Command::Sessions) => done(sessions::list()),
        Ok(Command::Diff(id)) => done(sessions::diff(&id)),
        Ok(Command::Apply(id)) => done(sessions::apply(&id)),
        Ok(Command::Discard(id)) => done(sessions::discard(&id)),
        Ok(Command::Run(options)) => options,
        Err(reason) => {
            eprint!("ferrion-run: {reason}\n\n{USAGE}");
            process::exit(EXIT_REFUSED);
        }
    };

    let mut withhold_network = options.no_network;
    let mut allow_sudo = false;
    if let Some(path) = profile_path(&options.program) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_profile(&text) {
                Ok(profile) => {
                    withhold_network |= !profile.network;
                    allow_sudo = profile.allow_sudo;
                }
                Err(e) => refuse(&format!("profile {}: {e}", path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => refuse(&format!("cannot read profile {}: {e}", path.display())),
        }
    }

    #[cfg(target_os = "redox")]
    match ferrion_run::launch::run(&options, withhold_network, allow_sudo) {
        ferrion_run::launch::Outcome::Refused(reason) => refuse(&reason),
        ferrion_run::launch::Outcome::Exited(code) => process::exit(code),
    }
    #[cfg(not(target_os = "redox"))]
    {
        let _ = (withhold_network, allow_sudo);
        refuse("runs on FerrionOS only");
    }
}
