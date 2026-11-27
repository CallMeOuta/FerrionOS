//! # Futex
//! Futex or Fast Userspace Mutex is "a method for waiting until a certain condition becomes true."
//!
//! For more information about futexes, please read [this](https://eli.thegreenplace.net/2018/basics-of-futexes/) blog post, and the [futex(2)](http://man7.org/linux/man-pages/man2/futex.2.html) man page
use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicU32, Ordering};
use hashbrown::{
    hash_map::{DefaultHashBuilder, Entry, OccupiedEntry},
    HashMap,
};
use rmm::Arch;
use syscall::EINTR;

use crate::{
    context::{
        self,
        memory::{AccessMode, AddrSpace, Provider},
        unblock_context, ContextLock,
    },
    memory::{Page, PhysicalAddress, VirtualAddress},
    sync::{CleanLockToken, Mutex, L1},
};

use crate::syscall::{
    data::TimeSpec,
    error::{Error, Result, EAGAIN, EFAULT, EINVAL, ENOMEM, ETIMEDOUT},
    flag::{FUTEX_WAIT, FUTEX_WAIT64, FUTEX_WAKE},
};

use super::usercopy::UserSlice;

/// Where a futex word lives, as waiters and wakers must agree on it.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum FutexKey {
    /// Memory private to one address space (`Provider::Allocated`): keyed by the address space
    /// and the virtual address. Its physical frame is not stable: after a fork the first write
    /// breaks copy-on-write and gives the writer a new frame, which would strand a thread that
    /// was already waiting on the old one.
    Private {
        addr_space: usize,
        addr: VirtualAddress,
    },
    /// Memory that stays shared between address spaces: keyed by physical address, so that
    /// waiters in other address spaces meet.
    Shared(PhysicalAddress),
}

type FutexList = HashMap<FutexKey, Vec<FutexEntry>>;

pub struct FutexEntry {
    // Context to wake up, and compare address spaces.
    context_lock: Weak<ContextLock>,
}

// TODO: Process-private futexes? In that case, put the futex table in each AddrSpace, or just
// implement that fully in userspace. Although futex is probably the best API for process-shared
// POSIX synchronization primitives, a local hash table and wait-for-thread kernel APIs (e.g.
// lwp_park/lwp_unpark from NetBSD) could be a simpler replacement.
static FUTEXES: Mutex<L1, FutexList> =
    Mutex::new(FutexList::with_hasher(DefaultHashBuilder::new()));

pub fn get_futex_stat(token: &mut CleanLockToken) -> (usize, usize) {
    let mut regc = 0;
    let mut regl = 0;
    let registry = FUTEXES.lock(token.token());
    for (_, v) in registry.iter() {
        regl += v.len();
        regc += 1;
    }
    (regc, regl)
}

fn validate_and_translate_virt(space: &AddrSpace, addr: VirtualAddress) -> Option<PhysicalAddress> {
    // TODO: Move this elsewhere!
    if addr.data().saturating_add(size_of::<usize>()) >= crate::USER_END_OFFSET {
        return None;
    }

    let page = Page::containing_address(addr);
    let off = addr.data() - page.start_address().data();

    let (frame, _) = space.table.utable.translate(page.start_address())?;

    Some(frame.add(off))
}

pub fn futex(
    addr: usize,
    op: usize,
    val: usize,
    val2: usize,
    _addr2: usize,
    token: &mut CleanLockToken,
) -> Result<usize> {
    let target_virtaddr = VirtualAddress::new(addr);
    let page = Page::containing_address(target_virtaddr);

    let timeout_opt = if matches!(op, FUTEX_WAIT | FUTEX_WAIT64) {
        UserSlice::ro(val2, size_of::<TimeSpec>())?
            .none_if_null()
            .map(|buf| unsafe { buf.read_exact::<TimeSpec>() })
            .transpose()?
    } else {
        None
    };

    let current_addrsp = AddrSpace::current()?;

    if op == FUTEX_WAKE {
        if target_virtaddr.data().saturating_add(size_of::<usize>()) >= crate::USER_END_OFFSET {
            return Err(Error::new(EFAULT));
        }
        let private_key = {
            let addr_space_guard = current_addrsp.acquire_read(token.downgrade());
            match addr_space_guard.grants.contains(page) {
                Some((_, info))
                    if matches!(info.provider, Provider::Allocated { .. })
                        && info.flags().has_user() =>
                {
                    Some(FutexKey::Private {
                        addr_space: Arc::as_ptr(&current_addrsp) as usize,
                        addr: target_virtaddr,
                    })
                }
                _ => None,
            }
        };
        if let Some(key) = private_key {
            return Ok(wake_futex(key, val, token));
        }
    }

    // Keep the address space locked so we can safely read from the physical address. Unlock it
    // before context switching.
    let addr_space_guard = current_addrsp.acquire_read(token.downgrade());
    let needs_correction = match addr_space_guard.grants.contains(page) {
        Some((_, info)) => matches!(info.provider, Provider::Allocated { .. }),
        None => false,
    };

    let addr_space_guard = if needs_correction {
        drop(addr_space_guard);
        crate::context::memory::try_correcting_page_tables(page, AccessMode::Write, token)
            .map_err(|err| match err {
                crate::context::memory::PfError::Oom => Error::new(ENOMEM),
                crate::context::memory::PfError::Segv => Error::new(EFAULT),
                _ => Error::new(EFAULT),
            })?;
        current_addrsp.acquire_read(token.downgrade())
    } else {
        addr_space_guard
    };

    let target_physaddr = validate_and_translate_virt(&addr_space_guard, target_virtaddr)
        .ok_or(Error::new(EFAULT))?;
    // `needs_correction` holds exactly for private (`Provider::Allocated`) memory.
    let key = if needs_correction {
        FutexKey::Private {
            addr_space: Arc::as_ptr(&current_addrsp) as usize,
            addr: target_virtaddr,
        }
    } else {
        FutexKey::Shared(target_physaddr)
    };

    match op {
        // TODO: FUTEX_WAIT_MULTIPLE?
        FUTEX_WAIT | FUTEX_WAIT64 => {
            // Must be aligned, otherwise it could cross a page boundary and mess up the
            // (simpler) validation we did in the first place.
            if op == FUTEX_WAIT && !addr.is_multiple_of(4) {
                return Err(Error::new(EINVAL));
            } else if op == FUTEX_WAIT64 && !addr.is_multiple_of(8) {
                return Err(Error::new(EINVAL));
            }

            {
                let context_lock = context::current();

                // Drop and block this context
                drop(addr_space_guard);

                {
                    let mut context = context_lock.write(token.token());

                    if context.being_sigkilled
                        || context.sigcontrol().is_some_and(|(tctl, pctl, _)| {
                            tctl.currently_pending_unblocked(pctl) != 0
                        })
                    {
                        return Err(Error::new(EINTR));
                    }
                    context.wake = timeout_opt.map(|time| time.to_nanos());
                    context.block("futex");
                    // Do not return early until rollback handled
                }

                let futexes = FUTEXES.lock(token.token());
                let (mut futexes, mut lock_token) = futexes.into_split();
                futexes
                    .entry(key)
                    .or_insert_with(Vec::new)
                    .push(FutexEntry {
                        context_lock: Arc::downgrade(&context_lock),
                    });

                // Now rearcquire again to read
                let addr_space_guard = current_addrsp.acquire_read(lock_token.downgrade());

                // Might be changing, read again to make sure
                let new_target_physaddr =
                    validate_and_translate_virt(&addr_space_guard, target_virtaddr);

                let (fetched, expected) = if let Some(target_physaddr) = new_target_physaddr {
                    if op == FUTEX_WAIT {
                        // TODO: On systems where virtual memory is not abundant, we might instead
                        // add an atomic usercopy function.
                        let accessible_addr =
                            crate::memory::RmmA::phys_to_virt(target_physaddr).data();

                        (
                            u64::from(unsafe {
                                (*(accessible_addr as *const AtomicU32)).load(Ordering::SeqCst)
                            }),
                            u64::from(val as u32),
                        )
                    } else {
                        #[cfg(target_has_atomic = "64")]
                        {
                            use core::sync::atomic::AtomicU64;

                            // TODO: On systems where virtual memory is not abundant, we might instead
                            // add an atomic usercopy function.
                            let accessible_addr =
                                crate::memory::RmmA::phys_to_virt(target_physaddr).data();

                            (
                                unsafe {
                                    (*(accessible_addr as *const AtomicU64)).load(Ordering::SeqCst)
                                },
                                val as u64,
                            )
                        }
                        #[cfg(not(target_has_atomic = "64"))]
                        {
                            // TODO: rollback
                            return Err(Error::new(crate::syscall::error::EOPNOTSUPP));
                        }
                    }
                } else {
                    (0, 1)
                };

                drop(addr_space_guard);

                // A shared word must still be in the frame it is keyed by; a private one is keyed
                // by its virtual address, and its value was just read from wherever it is now.
                let moved = match key {
                    FutexKey::Shared(_) => {
                        new_target_physaddr.is_none_or(|new| new != target_physaddr)
                    }
                    FutexKey::Private { .. } => new_target_physaddr.is_none(),
                };
                if moved || fetched != expected {
                    // The value or physical address is changed before switching, rollback
                    let futex = if let Entry::Occupied(futexes_entry) = futexes.entry(key) {
                        remove_futex_entry(futexes_entry)
                    } else {
                        None
                    };
                    drop(futexes);
                    if let Some(futex) = futex
                        && let Some(ctx) = futex.context_lock.upgrade()
                    {
                        unblock_context(&ctx, &mut token.token().downgrade());
                    }

                    context_lock.write(token.token()).wake = None;
                    return Err(Error::new(EAGAIN));
                }
            }

            context::switch(token);

            // The scheduler clears `wake` on timeout. Hence if a timeout was
            // set and `wake` is now `None`, we timed out.
            let pending_wake = context::current().write(token.token()).wake.take();
            let timed_out = timeout_opt.is_some() && pending_wake.is_none();

            // TODO: this doing a second search is not efficient. It probably best to have another
            // hashtable mapping context and whether it was woken up AND where to remove index from FUTEXES
            let mut futexes_lock = FUTEXES.lock(token.token());
            let (futexes_map, _) = futexes_lock.token_split();

            let Entry::Occupied(futexes) = futexes_map.entry(key) else {
                return Ok(0);
            };
            if remove_futex_entry(futexes).is_none() {
                return Ok(0);
            }

            if timed_out {
                return Err(Error::new(ETIMEDOUT));
            }

            Err(Error::new(EINTR))
        }
        FUTEX_WAKE => {
            drop(addr_space_guard);
            Ok(wake_futex(key, val, token))
        }
        _ => Err(Error::new(EINVAL)),
    }
}

fn wake_futex(key: FutexKey, val: usize, token: &mut CleanLockToken) -> usize {
    let mut woken = 0;
    let mut futexes_map = FUTEXES.lock(token.token());
    let (futexes_map, mut token) = futexes_map.token_split();

    if let Entry::Occupied(mut futexes_entry) = futexes_map.entry(key) {
        let futexes = futexes_entry.get_mut();
        while !futexes.is_empty() && woken < val {
            let entry = futexes.swap_remove(0);
            if let Some(ctx) = entry.context_lock.upgrade()
                && unblock_context(&ctx, &mut token.token().downgrade())
            {
                woken += 1;
            }
        }

        if futexes.is_empty() {
            futexes_entry.remove();
        }
    }

    woken
}

fn remove_futex_entry(
    mut futexes_entry: OccupiedEntry<'_, FutexKey, Vec<FutexEntry>>,
) -> Option<FutexEntry> {
    let current_lock = context::current();
    let futexes = futexes_entry.get_mut();
    let mut i = 0;
    while i < futexes.len() {
        if let Some(ctx) = futexes[i].context_lock.upgrade() {
            if Arc::ptr_eq(&ctx, &current_lock) {
                // Not woken up by FUTEX_WAKE, remove our entry
                let entry = futexes.swap_remove(i);
                if futexes.is_empty() {
                    futexes_entry.remove();
                }
                return Some(entry);
            }
            i += 1;
        } else {
            futexes.swap_remove(i);
        }
    }

    if futexes.is_empty() {
        futexes_entry.remove();
    }

    None
}
