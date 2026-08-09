//! The canonical alpha-compositing implementation.
//!
//! Before this crate existed there were six independent blend loops across
//! the kernel and userspace, and they disagreed about the *output* alpha:
//! the kernel's `resource::alpha_blend` forced `255`, while the syscall blit
//! path and `PixelBuffer` carried the source alpha through unchanged. Both
//! are wrong in general — one destroys the destination's transparency, the
//! other discards it.

/// Blend `src` over `dst` using the standard "source over" operator.
///
/// Both pixels are BGRA byte order (little-endian ARGB8888), which is what
/// the framebuffer and every client buffer use.
///
/// # Output alpha
///
/// The output alpha is `src_a + dst_a·(1 − src_a)`, which is the
/// mathematically correct Porter–Duff "over" result and the form this
/// codebase standardises on. It is the only form that composes: blending A
/// over B and then over C gives the same result as blending A over (B over
/// C). Blending onto an opaque destination is a special case of it — when
/// `dst_a` is 255 the formula yields 255 — so opaque-destination rendering
/// (the framebuffer) is unaffected, while blending into a *translucent*
/// intermediate buffer (which clients do, since they own their buffers)
/// stays correct too.
pub fn alpha_blend(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let src_alpha = src[3] as u32;

    if src_alpha == 255 {
        return src;
    }

    if src_alpha == 0 {
        return dst;
    }

    let inv_alpha = 255 - src_alpha;

    [
        ((src[0] as u32 * src_alpha + dst[0] as u32 * inv_alpha) / 255) as u8,
        ((src[1] as u32 * src_alpha + dst[1] as u32 * inv_alpha) / 255) as u8,
        ((src[2] as u32 * src_alpha + dst[2] as u32 * inv_alpha) / 255) as u8,
        (src_alpha + (dst[3] as u32 * inv_alpha) / 255) as u8,
    ]
}

/// Whether every pixel of a rectangular region of a BGRA buffer is fully
/// opaque. Bails out on the first non-opaque pixel.
///
/// The compositor uses this to choose the row-copy fast path over per-pixel
/// blending.
pub fn is_region_opaque(
    data: &[u8],
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    stride: u32,
) -> bool {
    for row in 0..height {
        let row_start = ((src_y + row) * stride + src_x) as usize * 4;
        for col in 0..width as usize {
            match data.get(row_start + col * 4 + 3) {
                Some(&255) => {}
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_source_replaces_destination() {
        assert_eq!(
            alpha_blend([1, 2, 3, 255], [9, 9, 9, 255]),
            [1, 2, 3, 255]
        );
    }

    #[test]
    fn transparent_source_keeps_destination() {
        assert_eq!(alpha_blend([1, 2, 3, 0], [9, 9, 9, 128]), [9, 9, 9, 128]);
    }

    #[test]
    fn half_alpha_over_opaque_destination_stays_opaque() {
        // Special case: an opaque destination must keep producing an
        // opaque result, so framebuffer rendering is unaffected.
        let out = alpha_blend([0, 0, 0, 128], [255, 255, 255, 255]);
        assert_eq!(out[3], 255);
        assert_eq!(out[0], 127);
    }

    #[test]
    fn half_alpha_over_translucent_destination_accumulates_alpha() {
        // 128 over 128 is 191, not 255 and not 128 — alpha accumulates
        // rather than saturating or averaging.
        let out = alpha_blend([0, 0, 0, 128], [0, 0, 0, 128]);
        assert_eq!(out[3], 191);
    }

    #[test]
    fn region_opacity_detects_a_single_transparent_pixel() {
        let mut data = [255u8; 4 * 4 * 4];
        assert!(is_region_opaque(&data, 0, 0, 4, 4, 4));
        data[(2 * 4 + 3) * 4 + 3] = 254;
        assert!(!is_region_opaque(&data, 0, 0, 4, 4, 4));
        // The changed pixel is outside this sub-region, which stays opaque.
        assert!(is_region_opaque(&data, 0, 0, 3, 3, 4));
    }

    #[test]
    fn region_opacity_rejects_a_truncated_buffer() {
        let data = [255u8; 8];
        assert!(!is_region_opaque(&data, 0, 0, 4, 4, 4));
    }
}
