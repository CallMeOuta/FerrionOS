use std::fmt;
use std::fs;
use std::process::ExitCode;
use std::ptr;

use libredox::error::Error;
use libredox::{call, flag, Fd};
use syscall::CallFlags;

const VENDOR: u16 = 0x1B36;
const DEVICE: u16 = 0x0005;
const PAGE: usize = 4096;
const BAR0_SIZE: usize = 0x1000;
const BAR0_UNCACHEABLE: u64 = 0b01 << 59;
const DMA_LENGTH: u32 = 64;
const FILL: u32 = 0x5A5A_5A5A;
const PATTERN: u32 = 0x1234_5678;
const RESULT_OK: u32 = 0;
const RESULT_REFUSED: u32 = 0xDEAD_0002;
const RESULT_BUSY: u32 = 0xFFFF_FFFE;
const MAX_RESULT_POLLS: usize = 1000;
const MAX_PCI_ENTRIES: usize = 256;
const DMA_VERB_ADDRESS: u64 = 1;
const DMA_VERB_RELEASE: u64 = 2;

const REG_TRIGGER: usize = 0x00;
const REG_GVA_LO: usize = 0x04;
const REG_GVA_HI: usize = 0x08;
const REG_LEN: usize = 0x0C;
const REG_RESULT: usize = 0x10;
const REG_DBELL: usize = 0x14;
const REG_ATTRS: usize = 0x18;
const REG_GPA_LO: usize = 0x1C;
const REG_GPA_HI: usize = 0x20;

#[derive(Clone, Copy)]
struct Address {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl Address {
    fn parse(name: &str) -> Option<Self> {
        let mut parts = name.split("--");
        let segment = u16::from_str_radix(parts.next()?, 16).ok()?;
        let bus = u8::from_str_radix(parts.next()?, 16).ok()?;
        let (device, function) = parts.next()?.split_once('.')?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            segment,
            bus,
            device: u8::from_str_radix(device, 16).ok()?,
            function: function.parse().ok()?,
        })
    }

    fn config_word(self, offset: u16) -> u64 {
        u64::from(self.segment) << 28
            | u64::from(self.bus) << 20
            | u64::from(self.device & 0x1F) << 15
            | u64::from(self.function & 0x7) << 12
            | u64::from(offset & 0xFFF)
    }

    fn channel_name(self) -> String {
        format!(
            "{:04x}--{:02x}--{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }

    fn handle_name(self) -> String {
        format!(
            "{:04x}-{:02x}-{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

struct Failure(String);

impl From<Error> for Failure {
    fn from(err: Error) -> Self {
        Failure(std::io::Error::from(err).to_string())
    }
}

fn fail<T>(text: impl Into<String>) -> Result<T, Failure> {
    Err(Failure(text.into()))
}

fn open(path: &str, flags: i32) -> Result<Fd, Error> {
    Fd::open(path, flags | flag::O_CLOEXEC, 0)
}

fn notify_ready() {
    if let Some(fd) = std::env::var("INIT_NOTIFY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        let _ = call::write(fd, &[0]);
        let _ = call::close(fd);
    }
}

fn find_device() -> Result<Address, Failure> {
    let access = open("/scheme/pci/access", flag::O_RDWR)?;
    let entries = fs::read_dir("/scheme/pci").map_err(|err| Failure(err.to_string()))?;
    for entry in entries.take(MAX_PCI_ENTRIES) {
        let entry = entry.map_err(|err| Failure(err.to_string()))?;
        let name = entry.file_name();
        let Some(address) = name.to_str().and_then(Address::parse) else {
            continue;
        };
        let mut ids = [0u8; 4];
        access.call_ro(&mut ids, CallFlags::empty(), &[1, address.config_word(0)])?;
        let vendor = u16::from_le_bytes([ids[0], ids[1]]);
        let device = u16::from_le_bytes([ids[2], ids[3]]);
        if vendor == VENDOR && device == DEVICE {
            return Ok(address);
        }
    }
    fail(format!(
        "no PCI function {VENDOR:04x}:{DEVICE:04x} in /scheme/pci"
    ))
}

fn channel(address: Address) -> Result<Fd, Failure> {
    if let Some(fd) = std::env::var("PCID_CLIENT_CHANNEL")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        return Ok(Fd::new(fd));
    }
    Ok(open(
        &format!("/scheme/pci/{}/channel", address.channel_name()),
        flag::O_RDWR,
    )?)
}

struct Registers(*mut u32);

impl Registers {
    fn map(channel: &Fd) -> Result<Self, Failure> {
        let base = unsafe {
            call::mmap(call::MmapArgs {
                addr: ptr::null_mut(),
                length: BAR0_SIZE,
                prot: flag::PROT_READ | flag::PROT_WRITE,
                flags: flag::MAP_SHARED,
                fd: channel.raw(),
                offset: BAR0_UNCACHEABLE,
            })
        }?;
        Ok(Self(base as *mut u32))
    }

    fn read(&self, offset: usize) -> u32 {
        assert!(offset % 4 == 0 && offset < BAR0_SIZE);
        unsafe { ptr::read_volatile(self.0.add(offset / 4)) }
    }

    fn write(&self, offset: usize, value: u32) {
        assert!(offset % 4 == 0 && offset < BAR0_SIZE);
        unsafe { ptr::write_volatile(self.0.add(offset / 4), value) }
    }

    fn dma_write(&self, device_address: u64, readback: u64) -> u32 {
        self.write(REG_GVA_LO, device_address as u32);
        self.write(REG_GVA_HI, (device_address >> 32) as u32);
        self.write(REG_GPA_LO, readback as u32);
        self.write(REG_GPA_HI, (readback >> 32) as u32);
        self.write(REG_LEN, DMA_LENGTH);
        self.write(REG_ATTRS, 0);
        self.write(REG_DBELL, 1);
        let _ = self.read(REG_TRIGGER);
        let mut result = self.read(REG_RESULT);
        for _ in 0..MAX_RESULT_POLLS {
            if result != RESULT_BUSY {
                break;
            }
            std::hint::spin_loop();
            result = self.read(REG_RESULT);
        }
        result
    }
}

struct Buffer {
    base: *mut u32,
    device_address: u64,
    physical: u64,
}

impl Buffer {
    fn fill(&self, value: u32) {
        for index in 0..PAGE / 4 {
            unsafe { ptr::write_volatile(self.base.add(index), value) };
        }
    }

    fn first_words(&self) -> [u32; 2] {
        unsafe {
            [
                ptr::read_volatile(self.base),
                ptr::read_volatile(self.base.add(1)),
            ]
        }
    }

    fn untouched(&self) -> bool {
        (0..PAGE / 4).all(|index| unsafe { ptr::read_volatile(self.base.add(index)) } == FILL)
    }
}

fn map_private(fd: &Fd, offset: u64) -> Result<*mut u32, Failure> {
    let base = unsafe {
        call::mmap(call::MmapArgs {
            addr: ptr::null_mut(),
            length: PAGE,
            prot: flag::PROT_READ | flag::PROT_WRITE,
            flags: flag::MAP_PRIVATE,
            fd: fd.raw(),
            offset,
        })
    }?;
    Ok(base as *mut u32)
}

fn translate(translation: &Fd, base: *mut u32) -> Result<u64, Failure> {
    let mut payload = (base as usize).to_ne_bytes();
    translation.call_rw(&mut payload, CallFlags::empty(), &[])?;
    Ok(usize::from_ne_bytes(payload) as u64)
}

fn old_path_buffer(translation: &Fd) -> Result<Buffer, Failure> {
    let memory = open("/scheme/memory/zeroed@wb?phys_contiguous", flag::O_RDWR)?;
    let base = map_private(&memory, 0)?;
    let physical = translate(translation, base)?;
    Ok(Buffer {
        base,
        device_address: physical,
        physical,
    })
}

fn handle_buffer(handle: &Fd, translation: &Fd) -> Result<Buffer, Failure> {
    let base = map_private(handle, 0)?;
    let mut payload = (base as usize).to_ne_bytes();
    handle.call_rw(&mut payload, CallFlags::empty(), &[DMA_VERB_ADDRESS])?;
    Ok(Buffer {
        base,
        device_address: usize::from_ne_bytes(payload) as u64,
        physical: translate(translation, base)?,
    })
}

fn release(handle: &Fd, buffer: &Buffer) -> Result<(), Failure> {
    let mut payload = [0u8; 16];
    payload[..8].copy_from_slice(&buffer.device_address.to_ne_bytes());
    payload[8..].copy_from_slice(&(PAGE as u64).to_ne_bytes());
    handle.call_wo(&payload, CallFlags::empty(), &[DMA_VERB_RELEASE])?;
    Ok(())
}

fn print_status(address: Address) {
    match fs::read_to_string("/scheme/sys/iommu") {
        Ok(text) => {
            for line in text.lines() {
                println!("iommu-probe: status: {line}");
            }
            if !text
                .lines()
                .any(|line| line.starts_with(&address.to_string()))
            {
                println!("iommu-probe: status lists no domain for {address}");
            }
        }
        Err(err) => println!("iommu-probe: status: unreadable: {err}"),
    }
}

fn run() -> Result<bool, Failure> {
    let address = find_device()?;
    let channel = channel(address)?;
    let registers = Registers::map(&channel)?;
    let translation = open("/scheme/memory/translation", flag::O_RDWR)?;
    println!("iommu-probe: device {address}");

    let handle = match open(
        &format!("/scheme/memory/dma/{}", address.handle_name()),
        flag::O_RDWR,
    ) {
        Ok(handle) => {
            println!("iommu-probe: handle: opened");
            Some(handle)
        }
        Err(err) => {
            println!(
                "iommu-probe: handle: refused ({}), buffer A takes the old path",
                std::io::Error::from(err)
            );
            None
        }
    };

    let a = match &handle {
        Some(handle) => handle_buffer(handle, &translation)?,
        None => old_path_buffer(&translation)?,
    };
    let b = old_path_buffer(&translation)?;
    a.fill(FILL);
    b.fill(FILL);
    println!(
        "iommu-probe: buffer A at {:#x} (physical {:#x})",
        a.device_address, a.physical
    );
    println!("iommu-probe: buffer B at {:#x}", b.physical);

    let result_a = registers.dma_write(a.device_address, a.physical);
    let result_b = registers.dma_write(b.device_address, b.physical);
    let a_words = a.first_words();
    let b_words = b.first_words();
    println!(
        "iommu-probe: device write to A: result {result_a:#x}, A starts {:#x} {:#x}",
        a_words[0], a_words[1]
    );
    println!(
        "iommu-probe: device write to B: result {result_b:#x}, B starts {:#x} {:#x}",
        b_words[0], b_words[1]
    );

    let granted = if result_a == RESULT_OK && a_words == [PATTERN, PATTERN] {
        "ok"
    } else {
        "failed"
    };
    let not_granted = if b.untouched() && result_b == RESULT_REFUSED {
        "blocked"
    } else if b_words[0] == PATTERN {
        "reached"
    } else {
        "unclear"
    };

    print_status(address);
    if let Some(handle) = &handle {
        match release(handle, &a) {
            Ok(()) => println!("iommu-probe: released A"),
            Err(Failure(err)) => println!("iommu-probe: release of A failed: {err}"),
        }
    }
    println!("iommu-probe: granted: {granted}, not granted: {not_granted}");
    Ok(granted == "ok" && not_granted == "blocked")
}

fn main() -> ExitCode {
    notify_ready();
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(Failure(err)) => {
            println!("iommu-probe: error: {err}");
            println!("iommu-probe: granted: error, not granted: error");
            ExitCode::from(2)
        }
    }
}
