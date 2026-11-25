use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, QueueHandle};

use crate::drag_out::DragImage;
use crate::platform::wayland_shm::{ShmRing, SlotAcquire};

const MAX_ICON_DIM: u32 = 1024;
const MAX_ICON_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct DragIcon {
    surface: WlSurface,
    ring: ShmRing,
}

impl DragIcon {
    pub(super) fn from_image<S>(
        compositor: &WlCompositor,
        shm: &WlShm,
        qh: &QueueHandle<S>,
        image: &DragImage,
    ) -> Option<Self>
    where
        S: Dispatch<WlSurface, ()> + Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, u64> + 'static,
    {
        if image.png_data().len() > MAX_ICON_BYTES {
            return None;
        }
        let rgba = crate::image::png_rgba8(image.png_data()).ok()?;
        let (width, height) = (rgba.width(), rgba.height());
        if width == 0 || height == 0 || width > MAX_ICON_DIM || height > MAX_ICON_DIM {
            return None;
        }
        let buf_scale = match image.points() {
            Some((pw, _)) if pw > 0.0 => ((width as f32 / pw).round() as i32).clamp(1, 8),
            _ => 1,
        };
        let surface = compositor.create_surface(qh, ());
        let mut ring = ShmRing::new(wl_shm::Format::Argb8888);
        let slot = match ring.acquire_slot(shm, qh, width, height, crate::time::now()) {
            Ok(SlotAcquire::Ready { slot, .. }) => slot,
            _ => {
                surface.destroy();
                return None;
            }
        };
        let Some((dst, w, h, stride)) = ring.slot_target_mut(slot) else {
            surface.destroy();
            return None;
        };
        assert_eq!(w, width);
        assert_eq!(h, height);
        let src = rgba.as_raw();
        let row_bytes = (width as usize).checked_mul(4)?;
        assert!(stride >= row_bytes);
        for row in 0..height as usize {
            let src_row = &src[row * row_bytes..(row + 1) * row_bytes];
            let dst_row = &mut dst[row * stride..row * stride + row_bytes];
            for (sp, dp) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                let a = u16::from(sp[3]);
                dp[0] = ((u16::from(sp[2]) * a + 127) / 255) as u8;
                dp[1] = ((u16::from(sp[1]) * a + 127) / 255) as u8;
                dp[2] = ((u16::from(sp[0]) * a + 127) / 255) as u8;
                dp[3] = sp[3];
            }
        }
        if ring
            .attach_and_damage(&surface, slot, &[[0, 0, width, height]], buf_scale, true)
            .is_err()
        {
            surface.destroy();
            return None;
        }
        surface.commit();
        ring.mark_presented(slot);
        Some(Self { surface, ring })
    }

    pub(super) fn surface(&self) -> &WlSurface {
        &self.surface
    }

    pub(super) fn mark_released(&mut self, id: u64) {
        self.ring.mark_released([id]);
    }
}

impl Drop for DragIcon {
    fn drop(&mut self) {
        self.surface.destroy();
    }
}
