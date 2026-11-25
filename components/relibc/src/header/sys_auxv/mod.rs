//! `sys/auxv.h` implementation.
//!
//! Non-POSIX, see <https://www.man7.org/linux/man-pages/man3/getauxval.3.html>.

use crate::platform::types::c_ulong;

pub use crate::platform::auxv_defs::*;

const fn is_standard_auxv_key(key: usize) -> bool {
    key != AT_NULL && key <= AT_EXECFN
}

/// See <https://www.man7.org/linux/man-pages/man3/getauxval.3.html>.
#[unsafe(no_mangle)]
pub extern "C" fn getauxval(t: c_ulong) -> c_ulong {
    let key = t as usize;
    if is_standard_auxv_key(key)
        && let Some(val) = crate::platform::get_auxv_saved(key)
    {
        return val as c_ulong;
    }
    crate::platform::ERRNO.set(crate::header::errno::ENOENT);
    0
}
