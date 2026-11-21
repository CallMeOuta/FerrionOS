use crate::platform::types::{c_int, c_void};
use alloc::vec::Vec;
use core::{cell::RefCell, ops::Range};
use spin::Mutex;

/// A callback registered with `atexit` or `__cxa_atexit`.
#[derive(Clone, Copy)]
enum ExitCallback {
    Plain(extern "C" fn()),
    WithArg(extern "C" fn(*mut c_void), usize),
}

#[derive(Clone, Copy)]
struct ExitFunc {
    callback: ExitCallback,
    /// The `__dso_handle` passed to `__cxa_atexit`; 0 for `atexit`, which has no handle.
    dso: usize,
}

impl ExitFunc {
    fn address(&self) -> usize {
        match self.callback {
            ExitCallback::Plain(func) => func as usize,
            ExitCallback::WithArg(func, _) => func as usize,
        }
    }

    fn run(self) {
        match self.callback {
            ExitCallback::Plain(func) => func(),
            ExitCallback::WithArg(func, arg) => func(arg as *mut c_void),
        }
    }
}

#[derive(Clone, Copy)]
struct CxaThreadAtExitFunc {
    func: extern "C" fn(*mut c_void),
    obj: *mut c_void,
    dso: *mut c_void,
}

/// Every exit callback still to run, in registration order.
static EXIT_FUNCS: Mutex<Vec<ExitFunc>> = Mutex::new(Vec::new());
#[thread_local]
static DTORS: RefCell<Vec<CxaThreadAtExitFunc>> = RefCell::new(Vec::new());

fn register(entry: ExitFunc) -> c_int {
    EXIT_FUNCS.lock().push(entry);
    0
}

/// Runs and removes the callbacks `selected` picks, last registered first.
/// The list is unlocked during each call, so a callback may register another;
/// one it registers that `selected` also picks runs next.
fn finalize_selected(selected: impl Fn(&ExitFunc) -> bool) {
    loop {
        let entry = {
            let mut funcs = EXIT_FUNCS.lock();
            let Some(index) = funcs.iter().rposition(&selected) else {
                return;
            };
            funcs.remove(index)
        };
        entry.run();
    }
}

/// Registers `func` for `exit`, as `atexit` does.
pub(crate) fn atexit(func: extern "C" fn()) -> c_int {
    register(ExitFunc {
        callback: ExitCallback::Plain(func),
        dso: 0,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_atexit(
    func: Option<extern "C" fn(*mut c_void)>,
    arg: *mut c_void,
    dso: *mut c_void,
) -> c_int {
    let Some(func) = func else {
        return 0;
    };
    register(ExitFunc {
        callback: ExitCallback::WithArg(func, arg as usize),
        dso: dso as usize,
    })
}

/// With a null `dso`, runs every exit callback, as `exit` does. Otherwise runs
/// those registered with that `__dso_handle`; a shared object's `crtbeginS.o`
/// calls this from its destructors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_finalize(dso: *mut c_void) {
    let dso = dso as usize;
    if dso == 0 {
        finalize_selected(|_| true);
    } else {
        finalize_selected(|entry| entry.dso == dso);
    }
}

/// Runs the exit callbacks of the shared object mapped at `range` before the
/// loader unmaps it. A callback belongs to the object when its `__dso_handle`
/// or its own address lies in the mapping. Plain `atexit` passes no handle,
/// because relibc's `libc.so` exports `atexit` itself instead of linking a
/// static stub into each object, so the callback's address is what ties it to
/// the object.
pub(crate) fn finalize_object(range: Range<usize>) {
    finalize_selected(|entry| range.contains(&entry.dso) || range.contains(&entry.address()));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_thread_atexit_impl(
    func: extern "C" fn(*mut c_void),
    obj: *mut c_void,
    dso: *mut c_void,
) {
    let entry = CxaThreadAtExitFunc { func, obj, dso };
    DTORS.borrow_mut().push(entry);
}

// called internally
pub unsafe fn __cxa_thread_finalize() {
    let mut dtors = DTORS.borrow_mut();
    while let Some(entry) = dtors.pop() {
        (entry.func)(entry.obj);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _ITM_deregisterTMCloneTable(_ptr: *mut c_void) {
    // No-op
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _ITM_registerTMCloneTable(_ptr: *mut c_void, _len: usize) {
    // No-op
}
