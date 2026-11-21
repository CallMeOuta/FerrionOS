use alloc::vec::Vec;
use core::{ptr, slice};

pub trait IoctlData {
    unsafe fn write(&self) -> Vec<u8>;
    unsafe fn read_from(&mut self, buf: &[u8]);
}

/// The layout of the payload a scheme receives for one request: the fixed
/// fields, which are the request's own fields without its array pointers,
/// then each array those pointers name, in this order.
pub struct WireLayout {
    pub fixed_size: usize,
    pub fixed: &'static [WireField],
    pub arrays: &'static [WireArray],
}

pub struct WireField {
    pub name: &'static str,
    pub offset: usize,
    pub size: usize,
}

pub struct WireArray {
    /// The request's field that points at the array.
    pub pointer: &'static str,
    /// The request's field that holds the number of elements.
    pub count: &'static str,
    pub element: &'static str,
    pub size: usize,
    pub align: usize,
    pub direction: WireDirection,
}

/// Which way an array's contents travel between the caller and the scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDirection {
    /// Sent to the scheme and copied back to the caller: `array`.
    Both,
    /// Sent to the scheme only: `in_array`.
    In,
    /// Sent as zeros and copied back to the caller: `out_array`.
    Out,
}

impl WireLayout {
    /// Whether the fixed fields cover the fixed part with no padding between them.
    pub const fn is_packed(&self) -> bool {
        let mut end = 0;
        let mut i = 0;
        while i < self.fixed.len() {
            if self.fixed[i].offset != end {
                return false;
            }
            end += self.fixed[i].size;
            i += 1;
        }
        end == self.fixed_size
    }

    /// Whether every array starts at an offset its elements' alignment divides,
    /// whatever the counts: the fixed part's size is a multiple of the first
    /// array's alignment, and no array is aligned more strictly than the one
    /// before it.
    pub const fn is_aligned(&self) -> bool {
        let mut i = 0;
        while i < self.arrays.len() {
            let align = self.arrays[i].align;
            if i == 0 && !self.fixed_size.is_multiple_of(align) {
                return false;
            }
            if i > 0 && align > self.arrays[i - 1].align {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// A typed view of one array inside a payload. The payload's bytes need not be
/// aligned for the element type: an array that is not is copied out, and
/// copied back when the view is dropped, if it was written. A payload the
/// caller sent with a write-only call is mapped read-only, so nothing is
/// written that the scheme did not change.
pub(crate) enum WireSlice<'a, T: Copy> {
    InPlace(&'a mut [T]),
    Copied {
        values: Vec<T>,
        bytes: &'a mut [u8],
        written: bool,
    },
}

impl<'a, T: Copy> WireSlice<'a, T> {
    /// # Safety
    ///
    /// `bytes` holds exactly `count` elements of `T`, and any bytes are a valid `T`.
    pub(crate) unsafe fn new(bytes: &'a mut [u8], count: usize) -> Self {
        debug_assert_eq!(Some(bytes.len()), count.checked_mul(size_of::<T>()));
        if bytes.as_ptr().cast::<T>().is_aligned() {
            Self::InPlace(unsafe {
                slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<T>(), count)
            })
        } else {
            let mut values = Vec::<T>::with_capacity(count);
            unsafe {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    values.as_mut_ptr().cast::<u8>(),
                    bytes.len(),
                );
                values.set_len(count);
            }
            Self::Copied {
                values,
                bytes,
                written: false,
            }
        }
    }

    pub(crate) fn get(&self) -> &[T] {
        match self {
            Self::InPlace(values) => values,
            Self::Copied { values, .. } => values,
        }
    }

    pub(crate) fn get_mut(&mut self) -> &mut [T] {
        match self {
            Self::InPlace(values) => values,
            Self::Copied {
                values, written, ..
            } => {
                *written = true;
                values
            }
        }
    }
}

impl<T: Copy> Drop for WireSlice<'_, T> {
    fn drop(&mut self) {
        if let Self::Copied {
            values,
            bytes,
            written: true,
        } = self
        {
            unsafe {
                ptr::copy_nonoverlapping(
                    values.as_ptr().cast::<u8>(),
                    bytes.as_mut_ptr(),
                    bytes.len(),
                );
            }
        }
    }
}

macro_rules! define_ioctl_data {
    (@direction array) => { $crate::WireDirection::Both };
    (@direction in_array) => { $crate::WireDirection::In };
    (@direction out_array) => { $crate::WireDirection::Out };

    (@write out_array, $data:ident, $ptr:expr, $size:ident) => {
        $data.extend(iter::repeat(0u8).take($size));
    };
    (@write $dir:ident, $data:ident, $ptr:expr, $size:ident) => {
        if $ptr as usize != 0 {
            let src = unsafe { slice::from_raw_parts($ptr as usize as *const u8, $size) };
            $data.extend_from_slice(src);
        } else {
            $data.extend(iter::repeat(0u8).take($size));
        };
    };

    (@read in_array, $dst:expr, $src:ident, $size:ident) => {
        let _ = $src;
    };
    (@read $dir:ident, $dst:expr, $src:ident, $size:ident) => {
        if $dst as usize != 0 {
            unsafe {
                slice::from_raw_parts_mut($dst as usize as *mut u8, $size).copy_from_slice($src);
            }
        }
    };

    (struct $ioctl_ty:ident, $mem_ty:ident {
        $($rest:tt)*
    }) => {
        define_ioctl_data!(
            struct $ioctl_ty, $mem_ty { $($rest)* } => (), (), ()
        );
    };
    (struct $ioctl_ty:ident, $mem_ty:ident {
        $field:ident: $ty:ty,
        $($rest:tt)*
    } =>
        ($($ioctl_fields:tt)*),
        ($($counted_fields:tt)*),
        ($($noncounted_fields:tt)*)
    ) => {
        define_ioctl_data!(
            struct $ioctl_ty, $mem_ty { $($rest)* } =>
                ($($ioctl_fields)* $field: $ty,),
                ($($counted_fields)*),
                ($($noncounted_fields)* $field: $ty,)
        );
    };
    (struct $ioctl_ty:ident, $mem_ty:ident {
        $field:ident: $ty:ty [$dir:ident<$el:ty, $counted_by:ident>],
        $($rest:tt)*
    } =>
        ($($ioctl_fields:tt)*),
        ($($counted_fields:tt)*),
        ($($noncounted_fields:tt)*)
    ) => {
        define_ioctl_data!(
            struct $ioctl_ty, $mem_ty { $($rest)* } =>
                ($($ioctl_fields)* $field: $ty,),
                ($($counted_fields)* $field: $ty [$dir<$el, $counted_by>],),
                ($($noncounted_fields)*)
        );
    };
    (struct $ioctl_ty:ident, $mem_ty:ident {} =>
        ($($ioctl_field:ident: $ioctl_field_ty:ty,)*),
        ($($counted_field:ident: $counted_ty:ty [$dir:ident<$el:ty, $counted_by:ident>],)*),
        ($($noncounted_field:ident: $noncounted_ty:ty,)*)
    ) => {
        // FIXME check ioctl_ty doesn't have padding
        const _: $ioctl_ty = $ioctl_ty {
            $($ioctl_field: unsafe { mem::zeroed::<$ioctl_field_ty>() },)*
        };

        #[repr(C)]
        pub struct ${concat(__, $mem_ty, Noncounted)} {
            $($noncounted_field: $noncounted_ty,)*
        }

        /// The scheme's view of one payload. The fixed fields are a copy, written
        /// back field by field when the view is dropped if a setter changed one,
        /// so the payload needs no alignment, its padding keeps the caller's
        /// bytes, and a payload sent with a write-only call, which the scheme
        /// sees read-only, is never written.
        pub struct $mem_ty<'a> {
            noncounted_fields: ${concat(__, $mem_ty, Noncounted)},
            noncounted_bytes: &'a mut [u8],
            written: bool,
            $($counted_field: $crate::ioctl_data::WireSlice<'a, $el>,)*
        }

        impl Drop for $mem_ty<'_> {
            fn drop(&mut self) {
                if !self.written {
                    return;
                }
                $(
                    let offset = ::core::mem::offset_of!(${concat(__, $mem_ty, Noncounted)}, $noncounted_field);
                    let field = &mut self.noncounted_bytes[offset..offset + size_of::<$noncounted_ty>()];
                    unsafe {
                        ::core::ptr::write_unaligned(
                            field.as_mut_ptr().cast::<$noncounted_ty>(),
                            self.noncounted_fields.$noncounted_field,
                        );
                    }
                )*
            }
        }

        impl $crate::ioctl_data::IoctlData for $ioctl_ty {
            unsafe fn write(&self) -> Vec<u8> {
                // Field by field into zeroed bytes, so that padding is sent as zeros.
                let mut data = Vec::<u8>::new();
                data.resize(size_of::<${concat(__, $mem_ty, Noncounted)}>(), 0);
                $(
                    let offset = ::core::mem::offset_of!(${concat(__, $mem_ty, Noncounted)}, $noncounted_field);
                    unsafe {
                        ::core::ptr::write_unaligned(
                            data[offset..].as_mut_ptr().cast::<$noncounted_ty>(),
                            self.$noncounted_field,
                        );
                    }
                )*
                $(
                    let size = self.$counted_by as usize * size_of::<$el>();
                    define_ioctl_data!(@write $dir, data, self.$counted_field, size);
                )*
                data
            }

            unsafe fn read_from(&mut self, mut buf: &[u8]) {
                // FIXME be robust against malicious scheme implementations by returning an error
                // when the buf is the wrong size
                let noncounted_fields = buf.split_off(..size_of::<${concat(__, $mem_ty, Noncounted)}>()).unwrap();

                $(
                    let size = self.$counted_by as usize * size_of::<$el>();
                    let $counted_field = buf.split_off(..size).unwrap();
                    define_ioctl_data!(@read $dir, self.$counted_field, $counted_field, size);
                )*

                assert!(buf.is_empty());

                let noncounted_fields = unsafe {
                    ::core::ptr::read_unaligned(noncounted_fields.as_ptr().cast::<${concat(__, $mem_ty, Noncounted)}>())
                };
                $(self.$noncounted_field = noncounted_fields.$noncounted_field;)*
            }
        }

        impl<'a> $mem_ty<'a> {
            pub const LAYOUT: $crate::WireLayout = $crate::WireLayout {
                fixed_size: size_of::<${concat(__, $mem_ty, Noncounted)}>(),
                fixed: &[$($crate::WireField {
                    name: stringify!($noncounted_field),
                    offset: ::core::mem::offset_of!(${concat(__, $mem_ty, Noncounted)}, $noncounted_field),
                    size: size_of::<$noncounted_ty>(),
                },)*],
                arrays: &[$($crate::WireArray {
                    pointer: stringify!($counted_field),
                    count: stringify!($counted_by),
                    element: stringify!($el),
                    size: size_of::<$el>(),
                    align: align_of::<$el>(),
                    direction: define_ioctl_data!(@direction $dir),
                },)*],
            };

            pub fn with(
                mut buf: &'a mut [u8],
                f: impl FnOnce($mem_ty<'a>) -> syscall::Result<usize>,
            ) -> syscall::Result<usize> {
                let noncounted_bytes = buf.split_off_mut(..size_of::<${concat(__, $mem_ty, Noncounted)}>())
                    .ok_or(syscall::Error::new(syscall::EINVAL))?;
                // Every field type is valid for any bytes.
                let noncounted_fields = unsafe {
                    ::core::ptr::read_unaligned(noncounted_bytes.as_ptr().cast::<${concat(__, $mem_ty, Noncounted)}>())
                };

                $(
                    let count = noncounted_fields.$counted_by as usize;
                    let len = count
                        .checked_mul(size_of::<$el>())
                        .ok_or(syscall::Error::new(syscall::EINVAL))?;
                    let $counted_field = buf.split_off_mut(..len)
                        .ok_or(syscall::Error::new(syscall::EINVAL))?;
                    // The bytes hold `count` elements, and every element type is valid for
                    // any bytes.
                    let $counted_field = unsafe { $crate::ioctl_data::WireSlice::new($counted_field, count) };
                )*

                if !buf.is_empty() {
                    return Err(syscall::Error::new(syscall::EINVAL));
                }

                f($mem_ty {
                    noncounted_fields,
                    noncounted_bytes,
                    written: false,
                    $($counted_field,)*
                })
            }

            $(
                pub fn $noncounted_field(&self) -> $noncounted_ty {
                    self.noncounted_fields.$noncounted_field
                }

                /// Should not be called for fields used as array length
                pub fn ${concat(set_, $noncounted_field)}(&mut self, data: $noncounted_ty) {
                    self.noncounted_fields.$noncounted_field = data;
                    self.written = true;
                }
            )*

            $(
                pub fn $counted_field(&self) -> &[$el] {
                    self.$counted_field.get()
                }

                /// The array as the caller sent it, to write in place. Its count
                /// stays as the caller sent it.
                pub fn ${concat($counted_field, _mut)}(&mut self) -> &mut [$el] {
                    self.$counted_field.get_mut()
                }

                /// Writes `data` and sets the count to its length, which the caller
                /// reads back, as for Linux's requests that answer how many
                /// elements there are.
                pub fn ${concat(set_, $counted_field)}(&mut self, data: &[$el]) {
                    let values = self.$counted_field.get_mut();
                    let copied_count = cmp::min(data.len(), values.len());
                    values[..copied_count].copy_from_slice(&data[..copied_count]);
                    self.noncounted_fields.$counted_by = data.len() as _;
                    self.written = true;
                }
            )*
        }
    };
}
