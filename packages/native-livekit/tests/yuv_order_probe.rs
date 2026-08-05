//! Pins the `yuv_helper` channel-order semantics the capture pipeline relies
//! on: the EGL readback delivers BGRA memory order (byte0=B, byte1=G,
//! byte2=R, byte3=A), and libyuv's ARGB layout is exactly that memory order.
//! If a future libwebrtc bump renames or swaps these helpers, this test fails
//! with a blue/red cast in every streamed frame.
//!
//! Expected BT.601 limited-range values for a pure-red pixel written in BGRA
//! memory order ([00,00,FF,FF]) interpreted as libyuv ARGB:
//!   Y = ((66*R + 129*G + 25*B + 128) >> 8) + 16 = 82
//!   U = ((-38*R - 74*G + 112*B + 128) >> 8) + 128 = 90
//!   V = ((112*R - 94*G - 18*B + 128) >> 8) + 128 = 239
#[cfg(target_os = "linux")]
#[test]
fn argb_to_i420_reads_bgra_memory_order() {
    use livekit::webrtc::native::yuv_helper;

    // A pure red pixel in BGRA memory order: B=0 G=0 R=255 A=255, 2x2.
    const RED_BGRA: [u8; 16] = [
        0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF,
        0xFF,
    ];

    let mut y = [0u8; 8];
    let mut u = [0u8; 4];
    let mut v = [0u8; 4];
    yuv_helper::argb_to_i420(&RED_BGRA, 8, &mut y, 4, &mut u, 2, &mut v, 2, 2, 2);

    assert_eq!(
        y[0], 82,
        "Y plane must match the ARGB-layout interpretation"
    );
    assert_eq!(y[1], 82);
    assert_eq!(
        u[0], 90,
        "U plane must match the ARGB-layout interpretation"
    );
    assert_eq!(
        v[0], 239,
        "V plane must match the ARGB-layout interpretation"
    );
}

/// `abgr_to_i420` (memory [R,G,B,A]) is NOT a substitute for the BGRA
/// readback: feeding BGRA memory to it would produce a red cast. Pinned so a
/// future refactor cannot "fix" the naming and break the colors.
#[cfg(target_os = "linux")]
#[test]
fn abgr_to_i420_does_not_match_bgra_memory_order() {
    use livekit::webrtc::native::yuv_helper;

    const RED_BGRA: [u8; 16] = [
        0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF,
        0xFF,
    ];

    let mut y = [0u8; 8];
    let mut u = [0u8; 4];
    let mut v = [0u8; 4];
    yuv_helper::abgr_to_i420(&RED_BGRA, 8, &mut y, 4, &mut u, 2, &mut v, 2, 2, 2);

    assert_ne!(y[0], 82, "ABGR would scramble the red pixel's Y");
    assert_ne!(v[0], 239, "ABGR would scramble the red pixel's V");
}

/// Pins the preview payload format: `i420_to_abgr` emits memory order
/// [R,G,B,A] — the only libyuv fourcc a `gl.RGBA` texture upload accepts
/// verbatim. If a future refactor switches to `i420_to_rgba` (memory
/// [A,B,G,R], the FOURCC "RGBA" layout) every preview frame gets a
/// red/blue swap. The preview path converts the downscaled I420 planes with
/// this exact call.
///
/// Assertions use the dominant-channel threshold (not exact equality) because
/// libyuv's fixed-point BT.601 conversion rounds chroma by a few counts.
#[cfg(target_os = "linux")]
#[test]
fn i420_to_abgr_matches_gl_rgba_memory_order() {
    use livekit::webrtc::native::yuv_helper;

    // Pure red in I420 (BT.601 limited range, same source as the ARGB probe
    // above): Y=82, U=90, V=239, 2x2 frame.
    let y = [82u8; 4];
    let u = [90u8];
    let v = [239u8];
    let mut rgba = [0u8; 16];
    yuv_helper::i420_to_abgr(&y, 2, &u, 1, &v, 1, &mut rgba, 8, 2, 2);

    for pixel in rgba.chunks_exact(4) {
        assert!(
            pixel[0] >= 250,
            "R channel must carry the red, got {}",
            pixel[0]
        );
        assert!(pixel[1] < 32, "G channel must stay dark, got {}", pixel[1]);
        assert!(pixel[2] < 32, "B channel must stay dark, got {}", pixel[2]);
        assert_eq!(pixel[3], 255, "alpha must be opaque");
    }

    // Pure blue: Y=41, U=240, V=110. The blue channel must land in the third
    // byte — a swapped output would put it in the first.
    let y = [41u8; 4];
    let u = [240u8];
    let v = [110u8];
    let mut rgba = [0u8; 16];
    yuv_helper::i420_to_abgr(&y, 2, &u, 1, &v, 1, &mut rgba, 8, 2, 2);

    for pixel in rgba.chunks_exact(4) {
        assert!(
            pixel[2] >= 250,
            "B channel must carry the blue, got {}",
            pixel[2]
        );
        assert!(pixel[0] < 32, "R channel must stay dark, got {}", pixel[0]);
        assert!(pixel[1] < 32, "G channel must stay dark, got {}", pixel[1]);
        assert_eq!(pixel[3], 255, "alpha must be opaque");
    }
}

/// libyuv's FOURCC "RGBA" is memory order [A,B,G,R], which a `gl.RGBA`
/// upload would read as blue-first — the trap that would swap every preview
/// frame. Pinned so nobody "simplifies" `i420_to_abgr` back to `i420_to_rgba`.
#[cfg(target_os = "linux")]
#[test]
fn i420_to_rgba_does_not_match_gl_rgba_memory_order() {
    use livekit::webrtc::native::yuv_helper;

    let y = [41u8; 4];
    let u = [240u8];
    let v = [110u8];
    let mut rgba = [0u8; 16];
    yuv_helper::i420_to_rgba(&y, 2, &u, 1, &v, 1, &mut rgba, 8, 2, 2);

    let pixel = &rgba[0..4];
    assert_ne!(
        &pixel[0..3],
        &[0, 0, 255],
        "I420ToRGBA's [A,B,G,R] memory order must not be used for gl.RGBA"
    );
    assert!(
        pixel[0] >= 250,
        "byte 0 carries the alpha in the RGBA fourcc"
    );
}
