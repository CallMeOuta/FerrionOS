#![no_std]
#![no_main]

use core::{mem, panic::PanicInfo, ptr, slice};

type Handle = usize;
type Status = usize;

const EFI_SUCCESS: Status = 0;
const EFI_INVALID_PARAMETER: Status = (1usize << 63) | 2;
const EFI_BUFFER_TOO_SMALL: Status = (1usize << 63) | 5;
const EFI_BOOT_SERVICES_DATA: u32 = 4;
const EFI_NATIVE_INTERFACE: u32 = 0;
const LOCATE_BY_PROTOCOL: u32 = 2;
const FILE_MODE_READ: u64 = 1;

const MAX_GOP_HANDLES: usize = 64;
const MAX_EDID_BYTES: usize = 4096;
const MAX_READ_STEPS: usize = 64;
const MAX_DEVICE_PATH_NODES: usize = 64;
const MAX_DEVICE_PATH_BYTES: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid(u32, u16, u16, [u8; 8]);

const LOADED_IMAGE_GUID: Guid = Guid(
    0x5b1b31a1,
    0x9562,
    0x11d2,
    [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const SIMPLE_FS_GUID: Guid = Guid(
    0x964e5b22,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const DEVICE_PATH_GUID: Guid = Guid(
    0x09576e91,
    0x6d3f,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const GOP_GUID: Guid = Guid(
    0x9042a9de,
    0x23dc,
    0x4a38,
    [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
);
const EDID_ACTIVE_GUID: Guid = Guid(
    0xbd8c1056,
    0x9f36,
    0x44ec,
    [0x92, 0xa8, 0xa6, 0x33, 0x7f, 0x81, 0x79, 0x86],
);
const EDID_DISCOVERED_GUID: Guid = Guid(
    0x1c0c34f6,
    0xd380,
    0x41fa,
    [0xa0, 0x49, 0x8a, 0xd0, 0x6c, 0x1a, 0x66, 0xaa],
);

#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

#[repr(C)]
struct SystemTable {
    hdr: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: Handle,
    console_in: usize,
    console_out_handle: Handle,
    console_out: usize,
    console_err_handle: Handle,
    console_err: usize,
    runtime_services: usize,
    boot_services: *mut BootServices,
}

#[repr(C)]
struct BootServices {
    hdr: TableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    allocate_pages: usize,
    free_pages: usize,
    get_memory_map: usize,
    allocate_pool: extern "efiapi" fn(u32, usize, *mut usize) -> Status,
    free_pool: extern "efiapi" fn(usize) -> Status,
    create_event: usize,
    set_timer: usize,
    wait_for_event: usize,
    signal_event: usize,
    close_event: usize,
    check_event: usize,
    install_protocol_interface: extern "efiapi" fn(*mut Handle, *const Guid, u32, usize) -> Status,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface: usize,
    handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut usize) -> Status,
    reserved: usize,
    register_protocol_notify: usize,
    locate_handle:
        extern "efiapi" fn(u32, *const Guid, *const usize, *mut usize, *mut Handle) -> Status,
    locate_device_path: usize,
    install_configuration_table: usize,
    load_image: extern "efiapi" fn(bool, Handle, usize, *const u8, usize, *mut Handle) -> Status,
    start_image: extern "efiapi" fn(Handle, *mut usize, *mut *mut u16) -> Status,
}

#[repr(C)]
struct LoadedImage {
    revision: u32,
    parent_handle: Handle,
    system_table: *mut SystemTable,
    device_handle: Handle,
    file_path: usize,
    reserved: usize,
    load_options_size: u32,
    load_options: *const u16,
    image_base: usize,
    image_size: u64,
    image_code_type: u32,
    image_data_type: u32,
    unload: usize,
}

#[repr(C)]
struct SimpleFileSystem {
    revision: u64,
    open_volume: extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

#[repr(C)]
struct FileProtocol {
    revision: u64,
    open: extern "efiapi" fn(
        *mut FileProtocol,
        *mut *mut FileProtocol,
        *const u16,
        u64,
        u64,
    ) -> Status,
    close: extern "efiapi" fn(*mut FileProtocol) -> Status,
    delete: usize,
    read: extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut u8) -> Status,
}

#[repr(C)]
struct EdidProtocol {
    size_of_edid: u32,
    edid: *const u8,
}

const EDID_FILE_PATH: [u16; 20] = [
    b'\\' as u16,
    b'E' as u16,
    b'F' as u16,
    b'I' as u16,
    b'\\' as u16,
    b'B' as u16,
    b'O' as u16,
    b'O' as u16,
    b'T' as u16,
    b'\\' as u16,
    b'T' as u16,
    b'E' as u16,
    b'S' as u16,
    b'T' as u16,
    b'.' as u16,
    b'E' as u16,
    b'D' as u16,
    b'I' as u16,
    b'D' as u16,
    0,
];

const CFG_FILE_PATH: [u16; 19] = [
    b'\\' as u16,
    b'E' as u16,
    b'F' as u16,
    b'I' as u16,
    b'\\' as u16,
    b'B' as u16,
    b'O' as u16,
    b'O' as u16,
    b'T' as u16,
    b'\\' as u16,
    b'T' as u16,
    b'E' as u16,
    b'S' as u16,
    b'T' as u16,
    b'.' as u16,
    b'C' as u16,
    b'F' as u16,
    b'G' as u16,
    0,
];

const TARGET_BOOTLOADER_PATH: [u16; 22] = [
    b'\\' as u16,
    b'E' as u16,
    b'F' as u16,
    b'I' as u16,
    b'\\' as u16,
    b'B' as u16,
    b'O' as u16,
    b'O' as u16,
    b'T' as u16,
    b'\\' as u16,
    b'F' as u16,
    b'E' as u16,
    b'R' as u16,
    b'R' as u16,
    b'I' as u16,
    b'O' as u16,
    b'N' as u16,
    b'.' as u16,
    b'E' as u16,
    b'F' as u16,
    b'I' as u16,
    0,
];

unsafe fn read_file(
    root: *mut FileProtocol,
    path: *const u16,
    out: &mut [u8],
) -> Result<usize, Status> {
    let mut file: *mut FileProtocol = ptr::null_mut();
    let st = unsafe { ((*root).open)(root, &mut file, path, FILE_MODE_READ, 0) };
    if st != EFI_SUCCESS {
        return Err(st);
    }
    let mut total = 0usize;
    for _ in 0..MAX_READ_STEPS {
        if total >= out.len() {
            break;
        }
        let mut chunk = out.len() - total;
        let rst = unsafe { ((*file).read)(file, &mut chunk, out.as_mut_ptr().add(total)) };
        if rst != EFI_SUCCESS {
            let _ = unsafe { ((*file).close)(file) };
            return Err(rst);
        }
        if chunk == 0 {
            break;
        }
        total += chunk;
    }
    let _ = unsafe { ((*file).close)(file) };
    Ok(total)
}

unsafe fn build_file_device_path(
    bs: &BootServices,
    device_handle: Handle,
    file_path: &[u16],
    out: &mut [u8; MAX_DEVICE_PATH_BYTES],
) -> Result<usize, Status> {
    let mut base_ptr = 0usize;
    let st = (bs.handle_protocol)(device_handle, &DEVICE_PATH_GUID, &mut base_ptr);
    if st != EFI_SUCCESS || base_ptr == 0 {
        return Err(if st != EFI_SUCCESS {
            st
        } else {
            EFI_INVALID_PARAMETER
        });
    }
    let base = base_ptr as *const u8;
    let mut prefix_len = 0usize;
    let mut found_end = false;
    for _ in 0..MAX_DEVICE_PATH_NODES {
        if prefix_len + 4 > MAX_DEVICE_PATH_BYTES {
            return Err(EFI_BUFFER_TOO_SMALL);
        }
        let node_type = unsafe { *base.add(prefix_len) };
        let node_subtype = unsafe { *base.add(prefix_len + 1) };
        let node_len = unsafe {
            u16::from_le_bytes([*base.add(prefix_len + 2), *base.add(prefix_len + 3)]) as usize
        };
        if node_len < 4 {
            return Err(EFI_INVALID_PARAMETER);
        }
        if node_type == 0x7F && node_subtype == 0xFF {
            found_end = true;
            break;
        }
        if prefix_len + node_len > MAX_DEVICE_PATH_BYTES {
            return Err(EFI_BUFFER_TOO_SMALL);
        }
        prefix_len += node_len;
    }
    if !found_end {
        return Err(EFI_BUFFER_TOO_SMALL);
    }
    let file_node_len = 4 + file_path.len() * 2;
    let total_len = prefix_len + file_node_len + 4;
    if total_len > MAX_DEVICE_PATH_BYTES || file_node_len > u16::MAX as usize {
        return Err(EFI_BUFFER_TOO_SMALL);
    }
    unsafe {
        ptr::copy_nonoverlapping(base, out.as_mut_ptr(), prefix_len);
    }
    let mut pos = prefix_len;
    out[pos] = 0x04;
    out[pos + 1] = 0x04;
    let len_bytes = (file_node_len as u16).to_le_bytes();
    out[pos + 2] = len_bytes[0];
    out[pos + 3] = len_bytes[1];
    pos += 4;
    for &ch in file_path {
        let b = ch.to_le_bytes();
        out[pos] = b[0];
        out[pos + 1] = b[1];
        pos += 2;
    }
    out[pos] = 0x7F;
    out[pos + 1] = 0xFF;
    out[pos + 2] = 0x04;
    out[pos + 3] = 0x00;
    pos += 4;
    Ok(pos)
}

#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: Handle, system_table: *mut SystemTable) -> Status {
    if system_table.is_null() {
        return EFI_INVALID_PARAMETER;
    }
    let bs_ptr = unsafe { (*system_table).boot_services };
    if bs_ptr.is_null() {
        return EFI_INVALID_PARAMETER;
    }
    let bs = unsafe { &*bs_ptr };

    let mut li_ptr = 0usize;
    let st = (bs.handle_protocol)(image_handle, &LOADED_IMAGE_GUID, &mut li_ptr);
    if st != EFI_SUCCESS || li_ptr == 0 {
        return st;
    }
    let device_handle = unsafe { (*(li_ptr as *const LoadedImage)).device_handle };

    let mut sfs_ptr = 0usize;
    let st = (bs.handle_protocol)(device_handle, &SIMPLE_FS_GUID, &mut sfs_ptr);
    if st != EFI_SUCCESS || sfs_ptr == 0 {
        return st;
    }
    let mut root: *mut FileProtocol = ptr::null_mut();
    let st = unsafe {
        ((*(sfs_ptr as *mut SimpleFileSystem)).open_volume)(
            sfs_ptr as *mut SimpleFileSystem,
            &mut root,
        )
    };
    if st != EFI_SUCCESS || root.is_null() {
        return st;
    }

    let mut edid_pool = 0usize;
    let st = (bs.allocate_pool)(EFI_BOOT_SERVICES_DATA, MAX_EDID_BYTES, &mut edid_pool);
    if st != EFI_SUCCESS || edid_pool == 0 {
        let _ = unsafe { ((*root).close)(root) };
        return st;
    }
    let edid_slice = unsafe { slice::from_raw_parts_mut(edid_pool as *mut u8, MAX_EDID_BYTES) };
    let edid_len = match unsafe { read_file(root, EDID_FILE_PATH.as_ptr(), edid_slice) } {
        Ok(n) => n,
        Err(err) => {
            let _ = unsafe { ((*root).close)(root) };
            return err;
        }
    };

    let mut cfg_buf = [0u8; 8];
    let target_guid = match unsafe { read_file(root, CFG_FILE_PATH.as_ptr(), &mut cfg_buf) } {
        Ok(n) if n > 0 && cfg_buf[0] == b'D' => EDID_DISCOVERED_GUID,
        _ => EDID_ACTIVE_GUID,
    };
    let _ = unsafe { ((*root).close)(root) };

    let mut proto_pool = 0usize;
    let st = (bs.allocate_pool)(
        EFI_BOOT_SERVICES_DATA,
        mem::size_of::<EdidProtocol>(),
        &mut proto_pool,
    );
    if st != EFI_SUCCESS || proto_pool == 0 {
        return st;
    }
    unsafe {
        ptr::write(
            proto_pool as *mut EdidProtocol,
            EdidProtocol {
                size_of_edid: edid_len as u32,
                edid: edid_pool as *const u8,
            },
        );
    }

    let mut handles = [0usize; MAX_GOP_HANDLES];
    let mut handles_bytes = handles.len() * mem::size_of::<Handle>();
    let st = (bs.locate_handle)(
        LOCATE_BY_PROTOCOL,
        &GOP_GUID,
        ptr::null(),
        &mut handles_bytes,
        handles.as_mut_ptr(),
    );
    if st == EFI_SUCCESS {
        let count = (handles_bytes / mem::size_of::<Handle>()).min(MAX_GOP_HANDLES);
        for i in 0..count {
            let mut h = handles[i];
            let _ = (bs.install_protocol_interface)(
                &mut h,
                &target_guid,
                EFI_NATIVE_INTERFACE,
                proto_pool,
            );
        }
    }

    let mut dev_path_buf = [0u8; MAX_DEVICE_PATH_BYTES];
    let path_res = unsafe {
        build_file_device_path(
            bs,
            device_handle,
            &TARGET_BOOTLOADER_PATH,
            &mut dev_path_buf,
        )
    };
    if let Err(err) = path_res {
        return err;
    }

    let mut child_handle: Handle = 0;
    let st = (bs.load_image)(
        false,
        image_handle,
        dev_path_buf.as_ptr() as usize,
        ptr::null(),
        0,
        &mut child_handle,
    );
    if st != EFI_SUCCESS {
        return st;
    }

    let mut exit_size = 0usize;
    let mut exit_data: *mut u16 = ptr::null_mut();
    (bs.start_image)(child_handle, &mut exit_size, &mut exit_data)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
