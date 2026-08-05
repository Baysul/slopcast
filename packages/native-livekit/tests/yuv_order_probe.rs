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
