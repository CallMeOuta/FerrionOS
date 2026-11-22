//! `sys/auxv.h` implementation.
//!
//! Non-POSIX, see <https://www.man7.org/linux/man-pages/man3/getauxval.3.html>.

use crate::platform::types::c_ulong;

pub use crate::platform::auxv_defs::*;

/// See <https://www.man7.org/linux/man-pages/man3/getauxval.3.html>.
#[unsafe(no_mangle)]
pub extern "C" fn getauxval(t: c_ulong) -> c_ulong {
    match crate::platform::get_auxv_saved(t as usize) {
        Some(val) => val as c_ulong,
        None => {
            crate::platform::ERRNO.set(crate::header::errno::ENOENT);
            0
        }
    }
}
