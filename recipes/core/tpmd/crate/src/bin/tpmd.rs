//! tpmd: the TPM 2.0 driver.
//!
//! It finds the TPM through the ACPI TPM2 table, maps its Command Response Buffer (CRB)
//! registers, checks it answers TPM2_GetRandom and mixes the bytes into /scheme/rand/hardware.
//! It registers no scheme name: it makes one
//! handle to itself, starts `attestd` with that handle and keeps none, so `attestd` is the only
//! process that can send the TPM a command, and no process can open another. When `attestd`
//! closes the handle, tpmd exits. Without a usable TPM it starts `attestd` with no handle.
//!
//! The register layout is the CRB interface of the TCG PC Client Platform TPM Profile (PTP)
//! specification; QEMU's `tpm-crb` device implements it.

use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;
use std::ptr;
use std::time::{Duration, Instant};

use libredox::Fd;
use libredox::call::MmapArgs;
use libredox::flag::{MAP_SHARED, O_CLOEXEC, O_RDWR, PROT_READ, PROT_WRITE};
use redox_scheme::scheme::SchemeSync;
use redox_scheme::{CallerCtx, Socket};
use syscall::data::Stat;
use syscall::error::{EBADF, EINVAL, EIO, EPERM, Error, Result};
use syscall::flag::MODE_CHR;
use tpmd::{TPM_FD_ENV, service, tpm2};

const PAGE_SIZE: usize = 4096;
const RANDOM_BYTES: u16 = 32;

/// ACPI TPM2 table: start method 7 is the Command Response Buffer interface.
const START_METHOD_CRB: u32 = 7;

// CRB registers of locality 0, as offsets from the start of the register page.
const LOC_STATE: usize = 0x00;
const LOC_CTRL: usize = 0x08;
/// The control area the TPM2 table points at sits at this offset of the register page.
const CTRL_AREA: usize = 0x40;
const CTRL_REQ: usize = 0x40;
const CTRL_STS: usize = 0x44;
const CTRL_CANCEL: usize = 0x48;
const CTRL_START: usize = 0x4C;
const CTRL_CMD_SIZE: usize = 0x58;
const CTRL_CMD_LADDR: usize = 0x5C;
const CTRL_CMD_HADDR: usize = 0x60;
const CTRL_RSP_SIZE: usize = 0x64;
const CTRL_RSP_ADDR: usize = 0x68;

const LOC_STATE_ASSIGNED: u32 = 1 << 1;
const LOC_STATE_REG_VALID: u32 = 1 << 7;
const LOC_CTRL_REQUEST_ACCESS: u32 = 1 << 0;
const CTRL_REQ_CMD_READY: u32 = 1 << 0;
const CTRL_REQ_GO_IDLE: u32 = 1 << 1;
const CTRL_STS_ERROR: u32 = 1 << 0;
const CTRL_START_INVOKE: u32 = 1 << 0;

/// How long locality and state changes may take (PTP TIMEOUT_C is 200 ms; Linux allows more).
const STATE_TIMEOUT: Duration = Duration::from_millis(750);
/// How long one command from attestd may take. Key creation is the slowest command allowed.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long each command of the probe may take; init waits for tpmd while it probes, so a TPM
/// that hangs must not hold up the boot for long.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The commands `attestd` may send. Everything that changes the TPM's state for later boots or
/// other users (PCR extends, NV storage, hierarchy control, clearing) is refused.
const ALLOWED_COMMANDS: [u32; 5] = [
    tpm2::CC_GET_RANDOM,
    tpm2::CC_PCR_READ,
    tpm2::CC_CREATE_PRIMARY,
    tpm2::CC_QUOTE,
    tpm2::CC_FLUSH_CONTEXT,
];

/// The number of the one handle tpmd hands out.
const HANDLE: usize = 1;

/// Maps `len` bytes of physical memory at `phys`, uncached.
fn physmap(phys: u64, len: usize) -> Result<*mut u8, String> {
    let page = phys & !(PAGE_SIZE as u64 - 1);
    let offset = (phys - page) as usize;
    let length = (offset + len).next_multiple_of(PAGE_SIZE);
    let fd = Fd::open("/scheme/memory/physical@uc", O_RDWR | O_CLOEXEC, 0)
        .map_err(|e| format!("cannot open physical memory: {e}"))?;
    let base = unsafe {
        libredox::call::mmap(MmapArgs {
            addr: ptr::null_mut(),
            length,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_SHARED,
            fd: fd.raw(),
            offset: page,
        })
    }
    .map_err(|e| format!("cannot map {phys:#x}: {e}"))?;
    Ok(unsafe { (base as *mut u8).add(offset) })
}

/// The TPM2 table from acpid, if the firmware has one.
fn tpm2_table() -> Result<Option<Vec<u8>>, String> {
    let dir = "/scheme/acpi/tables";
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot list {dir}: {e}"))?;
    for entry in entries {
        let name = entry
            .map_err(|e| format!("cannot list {dir}: {e}"))?
            .file_name();
        if name.to_string_lossy().starts_with("TPM2-") {
            let path = format!("{dir}/{}", name.to_string_lossy());
            return std::fs::read(&path)
                .map(Some)
                .map_err(|e| format!("cannot read {path}: {e}"));
        }
    }
    Ok(None)
}

struct Crb {
    regs: *mut u8,
    /// The locality registers exist when the control area sits at its PTP offset.
    has_locality: bool,
    cmd: *mut u8,
    cmd_size: usize,
    rsp: *mut u8,
    rsp_size: usize,
}

impl Crb {
    fn from_table(table: &[u8]) -> Result<Self, String> {
        if table.len() < 52 {
            return Err(format!("TPM2 table of {} bytes is too short", table.len()));
        }
        let ctrl_area = u64::from_le_bytes(table[40..48].try_into().unwrap());
        let start_method = u32::from_le_bytes(table[48..52].try_into().unwrap());
        if start_method != START_METHOD_CRB {
            return Err(format!(
                "TPM start method {start_method} is not supported, only the CRB interface (7)"
            ));
        }
        if ctrl_area == 0 {
            return Err("TPM2 table has no control area".into());
        }
        let page = ctrl_area & !(PAGE_SIZE as u64 - 1);
        let has_locality = ctrl_area - page == CTRL_AREA as u64;
        // Without locality registers, map the control area alone and address it as if it sat
        // at its PTP offset.
        let regs = if has_locality {
            physmap(page, PAGE_SIZE)?
        } else {
            unsafe { physmap(ctrl_area, PAGE_SIZE)?.sub(CTRL_AREA) }
        };
        let mut crb = Crb {
            regs,
            has_locality,
            cmd: ptr::null_mut(),
            cmd_size: 0,
            rsp: ptr::null_mut(),
            rsp_size: 0,
        };
        let cmd_phys =
            u64::from(crb.read(CTRL_CMD_HADDR)) << 32 | u64::from(crb.read(CTRL_CMD_LADDR));
        let rsp_phys =
            u64::from(crb.read(CTRL_RSP_ADDR + 4)) << 32 | u64::from(crb.read(CTRL_RSP_ADDR));
        crb.cmd_size = crb.read(CTRL_CMD_SIZE) as usize;
        crb.rsp_size = crb.read(CTRL_RSP_SIZE) as usize;
        if crb.cmd_size < tpm2::HEADER_LEN || crb.rsp_size < tpm2::HEADER_LEN {
            return Err("TPM reports no command or response buffer".into());
        }
        crb.cmd = physmap(cmd_phys, crb.cmd_size)?;
        crb.rsp = physmap(rsp_phys, crb.rsp_size)?;
        eprintln!(
            "tpmd: TPM 2.0 CRB at {page:#x}, command buffer {cmd_phys:#x} ({} bytes)",
            crb.cmd_size
        );
        Ok(crb)
    }

    fn read(&self, reg: usize) -> u32 {
        unsafe { ptr::read_volatile(self.regs.add(reg) as *const u32) }
    }

    fn write(&self, reg: usize, value: u32) {
        unsafe { ptr::write_volatile(self.regs.add(reg) as *mut u32, value) }
    }

    /// Waits until `reg & mask == want`.
    fn wait(&self, reg: usize, mask: u32, want: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.read(reg) & mask == want {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            // Yield like base's common::Timeout: after setrens(0, 0) tpmd has no time
            // scheme to sleep on, and a sleep never returns there.
            std::thread::yield_now();
        }
    }

    fn request_locality(&self) -> Result<(), String> {
        if !self.has_locality {
            return Ok(());
        }
        let want = LOC_STATE_ASSIGNED | LOC_STATE_REG_VALID;
        self.write(LOC_CTRL, LOC_CTRL_REQUEST_ACCESS);
        if !self.wait(LOC_STATE, want, want, STATE_TIMEOUT) {
            return Err("TPM did not grant locality 0".into());
        }
        Ok(())
    }

    fn transact(&self, command: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
        if command.len() > self.cmd_size {
            return Err(format!("command of {} bytes does not fit", command.len()));
        }
        if self.read(CTRL_STS) & CTRL_STS_ERROR != 0 {
            return Err("TPM is in a fatal error state".into());
        }
        self.write(CTRL_REQ, CTRL_REQ_CMD_READY);
        if !self.wait(CTRL_REQ, CTRL_REQ_CMD_READY, 0, STATE_TIMEOUT) {
            return Err("TPM did not become ready".into());
        }
        for (i, &byte) in command.iter().enumerate() {
            unsafe { ptr::write_volatile(self.cmd.add(i), byte) };
        }
        self.write(CTRL_START, CTRL_START_INVOKE);
        if !self.wait(CTRL_START, CTRL_START_INVOKE, 0, timeout) {
            self.write(CTRL_CANCEL, 1);
            self.wait(CTRL_START, CTRL_START_INVOKE, 0, STATE_TIMEOUT);
            self.write(CTRL_CANCEL, 0);
            return Err("TPM command timed out".into());
        }
        let read_byte = |i: usize| unsafe { ptr::read_volatile(self.rsp.add(i)) };
        let header: Vec<u8> = (0..tpm2::HEADER_LEN).map(read_byte).collect();
        let len = tpm2::declared_size(&header).unwrap();
        if !(tpm2::HEADER_LEN..=self.rsp_size).contains(&len) {
            return Err(format!("response size {len} is out of range"));
        }
        let response = (0..len).map(read_byte).collect();
        self.write(CTRL_REQ, CTRL_REQ_GO_IDLE);
        self.wait(CTRL_REQ, CTRL_REQ_GO_IDLE, 0, STATE_TIMEOUT);
        Ok(response)
    }

    /// Checks the TPM answers, sending TPM2_Startup first if the firmware did not.
    fn probe(&self) -> Result<Vec<u8>, String> {
        self.request_locality()?;
        let mut response = self.transact(&tpm2::get_random(RANDOM_BYTES), PROBE_TIMEOUT)?;
        if tpm2::response_code(&response) == Some(tpm2::RC_INITIALIZE) {
            eprintln!("tpmd: the firmware did not start the TPM; sending TPM2_Startup");
            let startup = self.transact(&tpm2::startup_clear(), PROBE_TIMEOUT)?;
            tpm2::parse_empty(&startup).map_err(|e| format!("TPM2_Startup: {e}"))?;
            response = self.transact(&tpm2::get_random(RANDOM_BYTES), PROBE_TIMEOUT)?;
        }
        let random =
            tpm2::parse_get_random(&response).map_err(|e| format!("TPM2_GetRandom: {e}"))?;
        if random.is_empty() || random.len() > usize::from(RANDOM_BYTES) {
            return Err(format!("TPM2_GetRandom returned {} bytes", random.len()));
        }
        for byte in &mut response {
            unsafe { ptr::write_volatile(byte, 0) };
        }
        Ok(random)
    }
}

struct TpmScheme {
    crb: Crb,
    /// The response to the last command, until `attestd` reads it.
    response: Option<Vec<u8>>,
}

impl SchemeSync for TpmScheme {
    fn write(
        &mut self,
        id: usize,
        buf: &[u8],
        _offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if id != HANDLE {
            return Err(Error::new(EBADF));
        }
        if tpm2::declared_size(buf) != Some(buf.len()) || buf.len() < tpm2::HEADER_LEN {
            return Err(Error::new(EINVAL));
        }
        let code = tpm2::command_code(buf).unwrap();
        if !ALLOWED_COMMANDS.contains(&code) {
            eprintln!("tpmd: refused command code {code:#x}");
            return Err(Error::new(EPERM));
        }
        let response = self.crb.transact(buf, COMMAND_TIMEOUT).map_err(|e| {
            eprintln!("tpmd: {e}");
            Error::new(EIO)
        })?;
        self.response = Some(response);
        Ok(buf.len())
    }

    fn read(
        &mut self,
        id: usize,
        buf: &mut [u8],
        _offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if id != HANDLE {
            return Err(Error::new(EBADF));
        }
        let Some(response) = self.response.take() else {
            return Ok(0);
        };
        if buf.len() < response.len() {
            self.response = Some(response);
            return Err(Error::new(EINVAL));
        }
        buf[..response.len()].copy_from_slice(&response);
        Ok(response.len())
    }

    fn fcntl(&mut self, _id: usize, _cmd: usize, _arg: usize, _ctx: &CallerCtx) -> Result<usize> {
        Ok(0)
    }

    fn fstat(&mut self, _id: usize, stat: &mut Stat, _ctx: &CallerCtx) -> Result<()> {
        stat.st_mode = MODE_CHR | 0o600;
        Ok(())
    }

    fn on_close(&mut self, id: usize) {
        if id == HANDLE {
            eprintln!("tpmd: attestd closed the TPM handle; exiting");
            std::process::exit(0);
        }
    }
}

/// Starts attestd with init's notification pipe and, if there is a TPM, the handle `tpm_fd`.
fn start_attestd(tpm_fd: Option<usize>) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/attestd");
    match tpm_fd {
        Some(fd) => command.env(TPM_FD_ENV, fd.to_string()),
        None => command.env_remove(TPM_FD_ENV),
    };
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot start attestd: {e}"))
}

/// Passes the probe's bytes to randd before entering the null namespace. A short TPM response
/// still contributes bytes, but randd only marks a hardware write seeded at 32 bytes.
fn feed_randd(random: &mut [u8]) {
    let result = OpenOptions::new()
        .write(true)
        .open("/scheme/rand/hardware")
        .and_then(|mut file| file.write_all(random));
    match result {
        Ok(()) => eprintln!(
            "tpmd: mixed {} bytes from TPM2_GetRandom into /scheme/rand",
            random.len()
        ),
        Err(err) => eprintln!("tpmd: cannot feed TPM2_GetRandom to /scheme/rand: {err}"),
    }
    for byte in random {
        unsafe { ptr::write_volatile(byte, 0) };
    }
}

fn main() {
    let notify = service::init_notify_fd().expect("tpmd: start it through init");

    let crb = tpm2_table().and_then(|table| match table {
        None => Err("no TPM2 ACPI table: no TPM".into()),
        Some(table) => {
            let crb = Crb::from_table(&table)?;
            let random = crb.probe()?;
            Ok((crb, random))
        }
    });
    let crb = match crb {
        Ok((crb, mut random)) => {
            feed_randd(&mut random);
            crb
        }
        Err(e) => {
            eprintln!("tpmd: {e}; attestd starts without a TPM");
            if let Err(e) = start_attestd(None) {
                eprintln!("tpmd: {e}");
            }
            std::process::exit(0);
        }
    };

    let socket = Socket::create().expect("tpmd: cannot create a scheme socket");
    let tpm_fd = socket
        .create_this_scheme_fd(0, HANDLE, syscall::flag::O_RDWR, 0)
        .expect("tpmd: cannot make the TPM handle");
    service::set_inherit(tpm_fd, true).expect("tpmd: cannot pass the TPM handle on");
    if let Err(e) = start_attestd(Some(tpm_fd)) {
        eprintln!("tpmd: {e}");
        std::process::exit(1);
    }
    // attestd holds the notification pipe and the handle now; tpmd keeps neither.
    let _ = libredox::call::close(tpm_fd);
    let _ = libredox::call::close(notify);
    libredox::call::setrens(0, 0).expect("tpmd: cannot enter the null namespace");

    let mut scheme = TpmScheme {
        crb,
        response: None,
    };
    if let Err(e) = service::serve(&socket, &mut scheme) {
        eprintln!("tpmd: {e}");
        std::process::exit(1);
    }
}
