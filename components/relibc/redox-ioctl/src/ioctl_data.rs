use alloc::vec::Vec;

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

        pub struct $mem_ty<'a> {
            noncounted_fields: &'a mut ${concat(__, $mem_ty, Noncounted)},
            $($counted_field: &'a mut [$el],)*
        }

        impl $crate::ioctl_data::IoctlData for $ioctl_ty {
            unsafe fn write(&self) -> Vec<u8> {
                let noncounted_fields = ${concat(__, $mem_ty, Noncounted)} {
                    $($noncounted_field: self.$noncounted_field,)*
                };
                // FIXME use Vec::with_capacity
                let mut data = Vec::<u8>::new();
                data.extend_from_slice(&unsafe {
                    mem::transmute::<
                        ${concat(__, $mem_ty, Noncounted)},
                        [u8; size_of::<${concat(__, $mem_ty, Noncounted)}>()],
                    >(noncounted_fields)
                });
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

                let noncounted_fields = unsafe { &*core::ptr::from_ref(noncounted_fields).cast::<${concat(__, $mem_ty, Noncounted)}>() };
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
                let noncounted_fields = buf.split_off_mut(..size_of::<${concat(__, $mem_ty, Noncounted)}>())
                    .ok_or(syscall::Error::new(syscall::EINVAL))?;
                let noncounted_fields = unsafe { &mut *core::ptr::from_mut(noncounted_fields).cast::<${concat(__, $mem_ty, Noncounted)}>() };

                $(
                    let $counted_field = buf.split_off_mut(..noncounted_fields.$counted_by as usize * size_of::<$el>())
                        .ok_or(syscall::Error::new(syscall::EINVAL))?;
                    let $counted_field = unsafe {
                        slice::from_raw_parts_mut(core::ptr::from_mut($counted_field).cast::<$el>(), noncounted_fields.$counted_by as usize)
                    };
                )*

                if !buf.is_empty() {
                    return Err(syscall::Error::new(syscall::EINVAL));
                }


                f($mem_ty {
                    noncounted_fields,
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
                }
            )*

            $(
                pub fn $counted_field(&self) -> &[$el] {
                    self.$counted_field
                }

                pub fn ${concat(set_, $counted_field)}(&mut self, data: &[$el]) {
                    let copied_count = cmp::min(data.len(), self.$counted_field.len());
                    self.$counted_field[..copied_count].copy_from_slice(&data[..copied_count]);
                    self.noncounted_fields.$counted_by = data.len() as _;
                }
            )*
        }
    };
}
