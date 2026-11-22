#[cfg(target_os = "redox")]
fn main() {
    let args: Vec<Option<u32>> = std::env::args()
        .skip(1)
        .map(|a| a.parse::<u32>().ok())
        .collect();
    let [Some(uid), Some(gid), Some(ready)] = args[..] else {
        eprintln!(
            "ferrion-scope-proxy: usage: ferrion-scope-proxy UID GID READY_FD (started by ferrion-scoped)"
        );
        std::process::exit(2);
    };
    let socket = match ferrion_scope::proxy::start(uid, gid, ready as usize) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("ferrion-scope-proxy: cannot start for uid {uid}: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = ferrion_scope::proxy::serve(socket, uid) {
        eprintln!("ferrion-scope-proxy: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "redox"))]
fn main() {
    eprintln!("ferrion-scope-proxy: runs on FerrionOS only");
    std::process::exit(2);
}
