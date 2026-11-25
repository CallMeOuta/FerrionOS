use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use glycin::{Loader, SandboxMechanism};
use glycin_utils::{
    ByteData, Frame, FrameRequest, ImageDetails, InitializationDetails, LoaderImplementation,
    MemoryFormat, ProcessError,
};

const MAX_PROBE_INPUT: usize = 4096;
const MAX_LISTENER_TICKS: usize = 6000;
const LISTENER_TICK_MS: u64 = 20;
const MAX_SHA_BLOCKS: usize = (256 << 20) / 64 + 4;

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let blocks = padded.len() / 64;
    assert!(blocks <= MAX_SHA_BLOCKS);
    for blk in 0..blocks {
        let base = blk * 64;
        let mut w = [0u32; 64];
        for i in 0..16 {
            let off = base + i * 4;
            w[i] = u32::from_be_bytes([
                padded[off],
                padded[off + 1],
                padded[off + 2],
                padded[off + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = String::with_capacity(64);
    for word in h {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{word:08x}");
    }
    out
}

fn sample_pixel(buf: &[u8], stride: usize, bpp: usize, x: usize, y: usize) -> String {
    let off = y
        .saturating_mul(stride)
        .saturating_add(x.saturating_mul(bpp));
    if off.saturating_add(bpp) > buf.len() {
        return "oob".into();
    }
    match bpp {
        4 => format!(
            "{},{},{},{}",
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3]
        ),
        3 => format!("{},{},{}", buf[off], buf[off + 1], buf[off + 2]),
        2 => format!("{},{}", buf[off], buf[off + 1]),
        1 => format!("{}", buf[off]),
        _ => format!(
            "{},{},{},{}",
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3]
        ),
    }
}

fn run_escape_attempts(
    secret_path: &str,
    tcp_addr: &str,
    unix_path: &str,
    http_path: &str,
) -> String {
    let secret_res = match std::fs::read(secret_path) {
        Ok(bytes) => format!("LEAKED({})", bytes.len()),
        Err(err) => format!("BLOCKED({})", err.raw_os_error().unwrap_or(-1)),
    };
    let tcp_res = match TcpStream::connect(tcp_addr) {
        Ok(mut stream) => {
            let req = format!("GET {http_path} HTTP/1.0\r\nHost: probe\r\n\r\n");
            let _ = stream.write_all(req.as_bytes());
            let mut reply = [0u8; 64];
            let _ = stream.read(&mut reply);
            "LEAKED".to_string()
        }
        Err(err) => format!("BLOCKED({})", err.raw_os_error().unwrap_or(-1)),
    };
    let unix_res = match UnixStream::connect(unix_path) {
        Ok(mut stream) => {
            let _ = stream.write_all(b"hi");
            "LEAKED".to_string()
        }
        Err(err) => format!("BLOCKED({})", err.raw_os_error().unwrap_or(-1)),
    };
    format!("secret={secret_res} tcp={tcp_res} unix={unix_res}")
}

struct EscapeProbeLoader {
    report: String,
    sent: bool,
}

impl LoaderImplementation for EscapeProbeLoader {
    fn load<B: ByteData, R: Read + Send + 'static>(
        mut stream: R,
        _mime_type: String,
        _details: InitializationDetails,
    ) -> Result<(Self, ImageDetails<B>), ProcessError> {
        let mut raw = vec![0u8; MAX_PROBE_INPUT];
        let mut total = 0usize;
        for _ in 0..64 {
            if total >= raw.len() {
                break;
            }
            match stream.read(&mut raw[total..]) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(ProcessError::expected(&err)),
            }
        }
        let text = String::from_utf8_lossy(&raw[..total]);
        let mut lines = text.lines();
        let _magic = lines.next().unwrap_or_default();
        let secret_path = lines.next().unwrap_or_default();
        let tcp_addr = lines.next().unwrap_or_default();
        let unix_path = lines.next().unwrap_or_default();
        let report = run_escape_attempts(secret_path, tcp_addr, unix_path, "/sandbox-leak");
        let mut details = ImageDetails::new(1, 1);
        details.info_format_name = Some(report.clone());
        Ok((
            Self {
                report,
                sent: false,
            },
            details,
        ))
    }

    fn specific_frame<B: ByteData>(
        &mut self,
        _frame_request: FrameRequest,
    ) -> Result<Frame<B>, ProcessError> {
        if self.sent {
            return Err(ProcessError::NoMoreFrames);
        }
        self.sent = true;
        let _ = &self.report;
        let texture = B::try_from_slice(&[0x12, 0x34, 0x56, 0xff])
            .map_err(|err| ProcessError::expected(&err))?;
        Frame::new(1, 1, MemoryFormat::R8g8b8a8, texture)
            .map_err(|err| ProcessError::expected(&err))
    }
}

fn cmd_load(path: &str, expect_fail: bool, expose_base_dir: bool) -> ExitCode {
    async_io::block_on(async {
        let start = Instant::now();
        let file = gio::File::for_path(path);
        let mut loader = Loader::new(file);
        loader.use_expose_base_dir(expose_base_dir);
        match loader.load().await {
            Ok(mut image) => {
                let mech = image.active_sandbox_mechanism();
                let mime = image.mime_type().to_string();
                match image.next_frame().await {
                    Ok(frame) => {
                        let elapsed_ms = start.elapsed().as_millis();
                        if expect_fail {
                            eprintln!("UNEXPECTED_LOAD_SUCCESS path={path} mime={mime}");
                            return ExitCode::FAILURE;
                        }
                        if mech != SandboxMechanism::FerrionRun {
                            eprintln!("WRONG_SANDBOX path={path} mech={mech:?}");
                            return ExitCode::FAILURE;
                        }
                        let w = frame.width();
                        let h = frame.height();
                        let stride = frame.stride();
                        let fmt = frame.memory_format();
                        let slice = frame.buf_slice();
                        let digest = sha256_hex(slice);
                        let bpp = (stride as usize) / (w.max(1) as usize);
                        let tl = sample_pixel(slice, stride as usize, bpp, 0, 0);
                        let mid = sample_pixel(
                            slice,
                            stride as usize,
                            bpp,
                            (w / 2) as usize,
                            (h / 2) as usize,
                        );
                        let br = sample_pixel(
                            slice,
                            stride as usize,
                            bpp,
                            w.saturating_sub(1) as usize,
                            h.saturating_sub(1) as usize,
                        );
                        println!(
                            "GLYCIN_LOAD_OK path={path} mime={mime} sandbox={mech:?} w={w} h={h} stride={stride} fmt={fmt:?} bytes={} ms={elapsed_ms} sha256={digest} tl={tl} mid={mid} br={br}",
                            slice.len()
                        );
                        ExitCode::SUCCESS
                    }
                    Err(err) => {
                        if expect_fail {
                            println!("GLYCIN_REFUSED path={path} stage=frame err={err}");
                            ExitCode::SUCCESS
                        } else {
                            eprintln!("GLYCIN_LOAD_FAIL path={path} stage=frame err={err}");
                            ExitCode::FAILURE
                        }
                    }
                }
            }
            Err(err) => {
                if expect_fail {
                    println!("GLYCIN_REFUSED path={path} stage=init err={err}");
                    ExitCode::SUCCESS
                } else {
                    eprintln!("GLYCIN_LOAD_FAIL path={path} stage=init err={err}");
                    ExitCode::FAILURE
                }
            }
        }
    })
}

fn cmd_supported() -> ExitCode {
    async_io::block_on(async {
        let mut types: Vec<String> = Loader::supported_mime_types()
            .await
            .into_iter()
            .map(|m| m.to_string())
            .collect();
        types.sort();
        for mime in types {
            println!("MIME {mime}");
        }
        ExitCode::SUCCESS
    })
}

fn cmd_unix_listener(sock_path: &str, count_path: &str, max_secs: u64) -> ExitCode {
    let _ = std::fs::remove_file(sock_path);
    let listener = match UnixListener::bind(sock_path) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("unix-listener bind {sock_path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("unix-listener nonblocking: {err}");
        return ExitCode::FAILURE;
    }
    let _ = std::fs::write(count_path, "0\n");
    println!("UNIX_LISTENER_READY {sock_path}");
    let _ = std::io::stdout().flush();

    let mut count: u64 = 0;
    let max_ticks = ((max_secs.saturating_mul(1000)) / LISTENER_TICK_MS)
        .min(MAX_LISTENER_TICKS as u64) as usize;
    for _ in 0..max_ticks {
        if !Path::new(sock_path).exists() {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = [0u8; 16];
                let _ = stream.read(&mut buf);
                count += 1;
                let _ = std::fs::write(count_path, format!("{count}\n"));
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(LISTENER_TICK_MS));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(LISTENER_TICK_MS));
            }
        }
    }
    let _ = std::fs::remove_file(sock_path);
    ExitCode::SUCCESS
}

fn cmd_sandbox_probe(input_path: &str, expose_base_dir: bool) -> ExitCode {
    async_io::block_on(async {
        let file = gio::File::for_path(input_path);
        let mut loader = Loader::new(file);
        loader.use_expose_base_dir(expose_base_dir);
        match loader.load().await {
            Ok(mut image) => {
                let mech = image.active_sandbox_mechanism();
                let report = image
                    .details()
                    .info_format_name()
                    .unwrap_or("missing")
                    .to_string();
                match image.next_frame().await {
                    Ok(frame) => {
                        println!(
                            "SANDBOX_PROBE_OK mech={mech:?} bytes={} {report}",
                            frame.buf_slice().len()
                        );
                        if mech == SandboxMechanism::FerrionRun
                            && report.contains("secret=BLOCKED")
                            && report.contains("tcp=BLOCKED")
                            && report.contains("unix=BLOCKED")
                            && frame.buf_slice() == [0x12, 0x34, 0x56, 0xff]
                        {
                            ExitCode::SUCCESS
                        } else {
                            ExitCode::FAILURE
                        }
                    }
                    Err(err) => {
                        eprintln!("SANDBOX_PROBE_FAIL stage=frame err={err}");
                        ExitCode::FAILURE
                    }
                }
            }
            Err(err) => {
                eprintln!("SANDBOX_PROBE_FAIL stage=init err={err}");
                ExitCode::FAILURE
            }
        }
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--dbus-fd") {
        glycin_utils::DbusServer::spawn_loader::<EscapeProbeLoader>(
            "glycin-load-probe-escape".into(),
        );
        return ExitCode::SUCCESS;
    }
    match args
        .iter()
        .skip(1)
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["load", path] => cmd_load(path, false, false),
        ["load", path, "--expect-fail"] => cmd_load(path, true, false),
        ["load", path, "--expose-base-dir"] => cmd_load(path, false, true),
        ["load", path, "--expose-base-dir", "--expect-fail"] => cmd_load(path, true, true),
        ["supported"] => cmd_supported(),
        ["unix-listener", sock_path, count_path, secs] => {
            cmd_unix_listener(sock_path, count_path, secs.parse().unwrap_or(60))
        }
        ["sandbox-control", secret_path, tcp_addr, unix_path] => {
            let report = run_escape_attempts(secret_path, tcp_addr, unix_path, "/control");
            println!("SANDBOX_CONTROL {report}");
            if report.contains("secret=LEAKED")
                && report.contains("tcp=LEAKED")
                && report.contains("unix=LEAKED")
            {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        ["sandbox-probe", input_path] => cmd_sandbox_probe(input_path, false),
        ["sandbox-probe", input_path, "--expose-base-dir"] => cmd_sandbox_probe(input_path, true),
        _ => {
            eprintln!(
                "usage: glycin-load-probe <load PATH [--expose-base-dir] [--expect-fail]|supported|unix-listener SOCK COUNT SECS|sandbox-control SECRET TCP UNIX|sandbox-probe INPUT [--expose-base-dir]>"
            );
            ExitCode::from(2)
        }
    }
}
