use std::path::Path;
use std::process;

use ferrion_run::manifest::{load_grants_file, load_manifest_for_exec, resolve_program_canonical};
use ferrion_run::{
    Command, EXIT_REFUSED, GRANTS_DIR, LaunchPolicy, MANIFEST_DIR, USAGE, manifest_policy,
    parse_args, parse_profile, profile_path, sessions,
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

fn resolve_policy(options: &ferrion_run::Options) -> Result<LaunchPolicy, String> {
    if let Some(resolved_exec) = resolve_program_canonical(&options.program)
        && let Some(manifest) = load_manifest_for_exec(Path::new(MANIFEST_DIR), &resolved_exec)?
    {
        let uid = unsafe { libc::geteuid() };
        let grants_path = Path::new(GRANTS_DIR).join(format!("{uid}.toml"));
        let grants = load_grants_file(&grants_path)?;
        let policy = manifest_policy(&manifest, &grants, options, resolved_exec)?;
        return Ok(LaunchPolicy::Manifest(policy));
    }

    let mut withhold_network = options.no_network;
    let mut allow_sudo = false;
    if let Some(path) = profile_path(&options.program) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_profile(&text) {
                Ok(profile) => {
                    eprintln!(
                        "ferrion-run: legacy profile {} (version 1) is deprecated; replace with a manifest",
                        path.display()
                    );
                    withhold_network |= !profile.network;
                    allow_sudo = profile.allow_sudo;
                }
                Err(e) => return Err(format!("profile {}: {e}", path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read profile {}: {e}", path.display())),
        }
    }
    Ok(LaunchPolicy::Legacy {
        withhold_network,
        allow_sudo,
    })
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

    let policy = match resolve_policy(&options) {
        Ok(policy) => policy,
        Err(reason) => refuse(&reason),
    };

    #[cfg(target_os = "redox")]
    match ferrion_run::launch::run(&options, &policy) {
        ferrion_run::launch::Outcome::Refused(reason) => refuse(&reason),
        ferrion_run::launch::Outcome::Exited(code) => process::exit(code),
    }
    #[cfg(not(target_os = "redox"))]
    {
        let _ = policy;
        refuse("runs on FerrionOS only");
    }
}
