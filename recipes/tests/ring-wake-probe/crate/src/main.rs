use libredox::call::{call_wo, clock_gettime, mmap, munmap, MmapArgs};
use libredox::{flag, Fd};
use redox_rings::op::{
    DiskOpCqe, DiskOpKind, DiskOpSqe, RingCallVerb, RingSetupFlags, RingSetupParams,
};
use redox_rings::raw::{RingPopError, INDEX_MASK};
use redox_rings::sync::{BlockingConsumer, BlockingProducer};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;
use std::ptr;
use std::sync::atomic::{fence, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use syscall::{CallFlags, TimeSpec};
use zerocopy::IntoBytes;

const RING_SQ_ENTRIES: u32 = 256;
const RING_POOL_BYTES: u32 = 64 * 4096;
const IO_LEN_BYTES: u32 = 4096;
const REQ_A_LBA: u64 = 0;
const REQ_B_LBA: u64 = 8;
const REQ_A_BUF_OFFSET: u32 = 0;
const REQ_B_BUF_OFFSET: u32 = IO_LEN_BYTES;
const MAX_DIR_ENTRIES: usize = 64;
const TSC_CALIBRATION_MS: u64 = 50;
const GAP_QUARTER_US_MAX: u64 = 120;
const ROUNDS_PER_GAP: usize = 20;
const MAX_ROUNDS: usize = ((GAP_QUARTER_US_MAX as usize) + 1) * ROUNDS_PER_GAP;
const MAX_RUN_SECS: u64 = 180;
const IDLE_SPIN_BEFORE_YIELD: usize = 1000;
const WAIT_IDLE_LIMIT_NS: u64 = 500_000_000;
const COMPLETION_DEADLINE_NS: u64 = 2_000_000_000;
const HOLD_ON_STUCK_SECS: u64 = 20;
const NS_PER_SEC: u64 = 1_000_000_000;

struct ProbeRing {
    sq: BlockingProducer<DiskOpSqe>,
    cq: BlockingConsumer<DiskOpCqe>,
    pipe_write: Fd,
    shm_ptr: *mut (),
    shm_len: usize,
    _ring_fd: Fd,
    _disk_fd: Fd,
}

impl Drop for ProbeRing {
    fn drop(&mut self) {
        if !self.shm_ptr.is_null() && self.shm_len > 0 {
            unsafe {
                let _ = munmap(self.shm_ptr, self.shm_len);
            }
        }
    }
}

impl ProbeRing {
    fn open(disk_path: &str) -> Result<Self, String> {
        assert!(REQ_B_BUF_OFFSET + IO_LEN_BYTES <= RING_POOL_BYTES);
        let disk_fd = Fd::open(disk_path, flag::O_RDONLY | flag::O_CLOEXEC, 0)
            .map_err(|err| format!("open {disk_path}: {err}"))?;
        let st = disk_fd
            .stat()
            .map_err(|err| format!("stat {disk_path}: {err}"))?;
        let sector_size = u64::try_from(st.st_blksize)
            .map_err(|_| format!("invalid st_blksize {}", st.st_blksize))?;
        if sector_size == 0 || (IO_LEN_BYTES as u64) % sector_size != 0 {
            return Err(format!(
                "unsupported sector size {sector_size} for {IO_LEN_BYTES}-byte reads"
            ));
        }
        let ring_raw = disk_fd
            .dup(b"uring")
            .map_err(|err| format!("dup uring on {disk_path}: {err}"))?;
        let ring_fd = Fd::new(ring_raw);
        let mut params = RingSetupParams {
            nr_sq_entries: RING_SQ_ENTRIES,
            nr_cq_entries: 0,
            flags: RingSetupFlags::empty().bits(),
            pool_size: RING_POOL_BYTES,
        };
        ring_fd
            .call_rw(
                params.as_mut_bytes(),
                CallFlags::empty(),
                &[RingCallVerb::Setup as u64],
            )
            .map_err(|err| format!("RingCallVerb::Setup: {err}"))?;
        let fixed_rtbl = [ring_fd.raw(), disk_fd.raw()];
        call_wo(
            fixed_rtbl.as_slice(),
            &[],
            CallFlags::empty(),
            &[RingCallVerb::SetFileTable as u64],
        )
        .map_err(|err| format!("RingCallVerb::SetFileTable: {err}"))?;
        let shm_ptr = unsafe {
            mmap(MmapArgs {
                addr: ptr::null_mut(),
                length: RING_POOL_BYTES as usize,
                prot: flag::PROT_READ | flag::PROT_WRITE,
                flags: flag::MAP_SHARED,
                fd: ring_fd.raw(),
                offset: 0,
            })
        }
        .map_err(|err| format!("mmap ring pool: {err}"))?;
        let mut fd_buf = [usize::MAX; 3];
        if let Err(err) = ring_fd.call_ro(
            fd_buf.as_mut_bytes(),
            CallFlags::FD | CallFlags::FD_UPPER,
            &[],
        ) {
            unsafe {
                let _ = munmap(shm_ptr, RING_POOL_BYTES as usize);
            }
            return Err(format!("call_ro ring fds: {err}"));
        }
        let sq_fd = Fd::new(fd_buf[0]);
        let cq_fd = Fd::new(fd_buf[1]);
        let pipe_read_fd = Fd::new(fd_buf[2]);
        let pipe_write = match pipe_read_fd.openat("write", 0, 0) {
            Ok(fd) => fd,
            Err(err) => {
                unsafe {
                    let _ = munmap(shm_ptr, RING_POOL_BYTES as usize);
                }
                return Err(format!("openat write on wake pipe: {err}"));
            }
        };
        drop(pipe_read_fd);
        let sq = match BlockingProducer::<DiskOpSqe>::from_fd(
            sq_fd,
            false,
            Some(params.nr_sq_entries),
        ) {
            Ok(sq) => sq,
            Err(err) => {
                unsafe {
                    let _ = munmap(shm_ptr, RING_POOL_BYTES as usize);
                }
                return Err(format!("SQ from_fd: {err}"));
            }
        };
        let cq = match BlockingConsumer::<DiskOpCqe>::from_fd(
            cq_fd,
            false,
            Some(params.nr_cq_entries),
        ) {
            Ok(cq) => cq,
            Err(err) => {
                unsafe {
                    let _ = munmap(shm_ptr, RING_POOL_BYTES as usize);
                }
                return Err(format!("CQ from_fd: {err}"));
            }
        };
        Ok(Self {
            sq,
            cq,
            pipe_write,
            shm_ptr,
            shm_len: RING_POOL_BYTES as usize,
            _ring_fd: ring_fd,
            _disk_fd: disk_fd,
        })
    }
}

fn ensure_unpartitioned_disk(disk_path: &str) -> Result<(), String> {
    let (scheme_dir, unit) = disk_path
        .rsplit_once('/')
        .ok_or_else(|| format!("disk path missing '/': {disk_path}"))?;
    if !scheme_dir.starts_with("/scheme/disk.") || unit.is_empty() {
        return Err(format!("unexpected disk path: {disk_path}"));
    }
    if unit.contains('p') {
        return Err(format!("refusing partition path: {disk_path}"));
    }
    let entries =
        fs::read_dir(scheme_dir).map_err(|err| format!("read_dir {scheme_dir}: {err}"))?;
    let mut seen = 0usize;
    let mut found_unit = false;
    for entry_res in entries {
        seen += 1;
        if seen > MAX_DIR_ENTRIES {
            return Err(format!("{scheme_dir} exceeded {MAX_DIR_ENTRIES} entries"));
        }
        let entry = entry_res.map_err(|err| format!("read_dir entry in {scheme_dir}: {err}"))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains('p') {
            return Err(format!(
                "refusing partitioned disk {scheme_dir}: found entry {name_str}"
            ));
        }
        if name_str == unit {
            found_unit = true;
        }
    }
    if !found_unit {
        return Err(format!("unit {unit} not listed in {scheme_dir}"));
    }
    Ok(())
}

fn clock_monotonic_ns() -> Result<u64, String> {
    let ts = clock_gettime(flag::CLOCK_MONOTONIC)
        .map_err(|err| format!("clock_gettime CLOCK_MONOTONIC: {err}"))?;
    if ts.tv_sec < 0 || ts.tv_nsec < 0 {
        return Err(format!(
            "negative monotonic time: {}.{}",
            ts.tv_sec, ts.tv_nsec
        ));
    }
    let sec = u64::try_from(ts.tv_sec).map_err(|_| "monotonic sec overflow".to_string())?;
    let nsec = u64::try_from(ts.tv_nsec).map_err(|_| "monotonic nsec overflow".to_string())?;
    sec.checked_mul(NS_PER_SEC)
        .and_then(|v| v.checked_add(nsec))
        .ok_or_else(|| "monotonic ns overflow".to_string())
}

fn monotonic_deadline(offset_ns: u64) -> Result<TimeSpec, String> {
    let target_ns = clock_monotonic_ns()?
        .checked_add(offset_ns)
        .ok_or_else(|| "deadline ns overflow".to_string())?;
    let tv_sec =
        i64::try_from(target_ns / NS_PER_SEC).map_err(|_| "deadline sec overflow".to_string())?;
    let tv_nsec =
        i32::try_from(target_ns % NS_PER_SEC).map_err(|_| "deadline nsec overflow".to_string())?;
    Ok(TimeSpec { tv_sec, tv_nsec })
}

#[cfg(target_arch = "x86_64")]
fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_tsc() -> u64 {
    clock_monotonic_ns().unwrap_or(0)
}

fn calibrate_tsc() -> Result<(u64, u64), String> {
    let t0 = clock_monotonic_ns()?;
    let c0 = read_tsc();
    thread::sleep(Duration::from_millis(TSC_CALIBRATION_MS));
    let c1 = read_tsc();
    let t1 = clock_monotonic_ns()?;
    let delta_ns = t1.saturating_sub(t0);
    let delta_tsc = c1.wrapping_sub(c0);
    if delta_ns == 0 || delta_tsc == 0 {
        return Err(format!(
            "invalid TSC calibration delta_ns={delta_ns} delta_tsc={delta_tsc}"
        ));
    }
    Ok((delta_tsc, delta_ns))
}

fn gap_ticks(gap_quarter_us: u64, delta_tsc: u64, delta_ns: u64) -> u64 {
    assert!(delta_ns > 0);
    let target_ns = u128::from(gap_quarter_us) * 250u128;
    let ticks = (target_ns * u128::from(delta_tsc)) / u128::from(delta_ns);
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

fn write_wake(pipe_write: &Fd) -> Result<(), String> {
    let written = pipe_write
        .write(&[0u8])
        .map_err(|err| format!("write wake byte: {err}"))?;
    if written != 1 {
        return Err(format!("short wake write: {written}"));
    }
    Ok(())
}

fn make_read_sqe(block: u64, buf_offset: u32, user_data: u64) -> DiskOpSqe {
    DiskOpSqe {
        opcode: DiskOpKind::Read as u8,
        pad: [0; 3],
        file_idx: 0,
        block,
        buf_offset,
        buf_len: IO_LEN_BYTES,
        user_data,
    }
}

fn wait_for_idle(sq: &BlockingProducer<DiskOpSqe>) -> Result<(), String> {
    let deadline_ns = clock_monotonic_ns()? + WAIT_IDLE_LIMIT_NS;
    let mut spins = 0usize;
    loop {
        if sq.inner.header.is_wait_head() {
            return Ok(());
        }
        if spins < IDLE_SPIN_BEFORE_YIELD {
            core::hint::spin_loop();
            spins += 1;
        } else {
            if clock_monotonic_ns()? >= deadline_ns {
                return Err("timed out waiting for ring worker idle state".to_string());
            }
            thread::yield_now();
            spins = 0;
        }
    }
}

fn wait_for_head_advance(
    sq: &BlockingProducer<DiskOpSqe>,
    head_before: u32,
) -> Result<bool, String> {
    let deadline_ns = clock_monotonic_ns()? + COMPLETION_DEADLINE_NS;
    let mut spins = 0usize;
    loop {
        let head_now = sq.inner.header.head.load(Ordering::Acquire) & INDEX_MASK;
        if head_now != head_before {
            return Ok(true);
        }
        if spins < IDLE_SPIN_BEFORE_YIELD {
            core::hint::spin_loop();
            spins += 1;
        } else {
            if clock_monotonic_ns()? >= deadline_ns {
                return Ok(false);
            }
            spins = 0;
        }
    }
}

enum ProbeOutcome {
    Pass,
    Stuck,
    BadCqe,
}

fn run_probe(disk_path: &str) -> Result<ProbeOutcome, String> {
    ensure_unpartitioned_disk(disk_path)?;
    let (delta_tsc, delta_ns) = calibrate_tsc()?;
    let ticks_per_us = (delta_tsc as f64) * 1000.0 / (delta_ns as f64);
    println!("RINGWAKE-TSC ticks_per_us={ticks_per_us:.2}");
    let _ = io::stdout().flush();

    let mut ring = ProbeRing::open(disk_path)?;
    let run_start = Instant::now();
    let mut next_id = 1u64;
    let mut total_rounds = 0usize;
    let mut total_bit_clear_a = 0usize;
    let mut total_bit_clear = 0usize;
    let mut total_bad_cqe = 0usize;
    let mut overall_worst_us = 0u64;

    for gap_quarter_us in 0..=GAP_QUARTER_US_MAX {
        let gap_us = (gap_quarter_us as f64) * 0.25;
        let wait_ticks = gap_ticks(gap_quarter_us, delta_tsc, delta_ns);
        let mut gap_ok = 0usize;
        let mut gap_bit_clear_a = 0usize;
        let mut gap_bit_clear = 0usize;
        let mut gap_bad_cqe = 0usize;
        let mut gap_worst_us = 0u64;

        for _ in 0..ROUNDS_PER_GAP {
            if run_start.elapsed() >= Duration::from_secs(MAX_RUN_SECS) {
                return Err(format!(
                    "whole-run bound {MAX_RUN_SECS}s reached at round {total_rounds} gap_us={gap_us:.2}"
                ));
            }
            assert!(total_rounds < MAX_ROUNDS);
            wait_for_idle(&ring.sq)?;

            let round_start_ns = clock_monotonic_ns()?;
            let head_before = ring.sq.inner.header.head.load(Ordering::Acquire) & INDEX_MASK;

            let id_a = next_id;
            next_id += 1;
            let sqe_a = make_read_sqe(REQ_A_LBA, REQ_A_BUF_OFFSET, id_a);
            ring.sq
                .inner
                .push_back(sqe_a)
                .map_err(|err| format!("push_back A round {total_rounds}: {err}"))?;
            fence(Ordering::SeqCst);
            let waited_a = ring.sq.inner.header.is_wait_head();
            if waited_a {
                write_wake(&ring.pipe_write)?;
            } else {
                gap_bit_clear_a += 1;
                total_bit_clear_a += 1;
            }

            if !wait_for_head_advance(&ring.sq, head_before)? {
                println!(
                    "RINGWAKE-STUCK round={total_rounds} gap_us={gap_us:.2} bit_clear_a={total_bit_clear_a} bit_clear={total_bit_clear} stage=pop_a waited_a={} waited_b=0 got_a=0 got_b=0",
                    u8::from(waited_a)
                );
                let _ = io::stdout().flush();
                thread::sleep(Duration::from_secs(HOLD_ON_STUCK_SECS));
                return Ok(ProbeOutcome::Stuck);
            }

            let pop_tsc = read_tsc();
            while read_tsc().wrapping_sub(pop_tsc) < wait_ticks {
                core::hint::spin_loop();
            }

            let id_b = next_id;
            next_id += 1;
            let sqe_b = make_read_sqe(REQ_B_LBA, REQ_B_BUF_OFFSET, id_b);
            ring.sq
                .inner
                .push_back(sqe_b)
                .map_err(|err| format!("push_back B round {total_rounds}: {err}"))?;
            fence(Ordering::SeqCst);
            let waited_b = ring.sq.inner.header.is_wait_head();
            if waited_b {
                write_wake(&ring.pipe_write)?;
            } else {
                gap_bit_clear += 1;
                total_bit_clear += 1;
            }

            let deadline = monotonic_deadline(COMPLETION_DEADLINE_NS)?;
            let mut got_a = false;
            let mut got_b = false;
            let mut round_bad_cqe = false;
            for _ in 0..2 {
                match ring.cq.pop(Some(&deadline)) {
                    Ok(cqe) => {
                        if cqe.status != 0 || cqe.count != IO_LEN_BYTES {
                            gap_bad_cqe += 1;
                            total_bad_cqe += 1;
                            round_bad_cqe = true;
                            println!(
                                "RINGWAKE-BADCQE round={total_rounds} gap={gap_us:.2} status={} count={}",
                                cqe.status, cqe.count
                            );
                            let _ = io::stdout().flush();
                        }
                        if cqe.user_data == id_a && !got_a {
                            got_a = true;
                        } else if cqe.user_data == id_b && !got_b {
                            got_b = true;
                        } else {
                            return Err(format!(
                                "unexpected CQE user_data={} (expected {id_a} or {id_b})",
                                cqe.user_data
                            ));
                        }
                    }
                    Err(RingPopError::Empty) | Err(RingPopError::Broken) => {
                        println!(
                            "RINGWAKE-STUCK round={total_rounds} gap_us={gap_us:.2} bit_clear_a={total_bit_clear_a} bit_clear={total_bit_clear} stage=cqe waited_a={} waited_b={} got_a={} got_b={}",
                            u8::from(waited_a),
                            u8::from(waited_b),
                            u8::from(got_a),
                            u8::from(got_b)
                        );
                        let _ = io::stdout().flush();
                        thread::sleep(Duration::from_secs(HOLD_ON_STUCK_SECS));
                        return Ok(ProbeOutcome::Stuck);
                    }
                }
            }
            assert!(got_a && got_b);

            let round_us = clock_monotonic_ns()?.saturating_sub(round_start_ns) / 1000;
            if round_us > gap_worst_us {
                gap_worst_us = round_us;
            }
            if round_us > overall_worst_us {
                overall_worst_us = round_us;
            }
            if !round_bad_cqe {
                gap_ok += 1;
            }
            total_rounds += 1;
        }

        println!(
            "RINGWAKE-GAP gap_us={gap_us:.2} ok={gap_ok} bit_clear_a={gap_bit_clear_a} bit_clear={gap_bit_clear} bad_cqe={gap_bad_cqe} worst_us={gap_worst_us}"
        );
        let _ = io::stdout().flush();
    }

    assert_eq!(total_rounds, MAX_ROUNDS);
    println!(
        "RINGWAKE-DONE rounds={total_rounds} bit_clear_a={total_bit_clear_a} bit_clear={total_bit_clear} bad_cqe={total_bad_cqe} worst_us={overall_worst_us}"
    );
    let _ = io::stdout().flush();
    if total_bad_cqe > 0 {
        return Ok(ProbeOutcome::BadCqe);
    }
    Ok(ProbeOutcome::Pass)
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(disk_path) = args.next() else {
        eprintln!("RINGWAKE-ERROR usage: ring-wake-probe <disk_path>");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("RINGWAKE-ERROR unexpected extra argument");
        return ExitCode::from(2);
    }
    match run_probe(&disk_path) {
        Ok(ProbeOutcome::Pass) => ExitCode::SUCCESS,
        Ok(ProbeOutcome::Stuck) => ExitCode::from(1),
        Ok(ProbeOutcome::BadCqe) => ExitCode::from(3),
        Err(err) => {
            eprintln!("RINGWAKE-ERROR {err}");
            ExitCode::from(2)
        }
    }
}
