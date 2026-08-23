//! Direct GStreamer-to-LiveKit compatibility probe.
//!
//! Publishes an explicitly encoded H.264 test pattern through
//! `livekitwebrtcsink`, bypassing the LiveKit Rust SDK's libwebrtc publisher.

#[cfg(target_os = "linux")]
mod linux {
    use std::error::Error;
    use std::fmt::{self, Display, Formatter};
    use std::time::{Duration, Instant};

    use gst::prelude::*;
    use gstreamer as gst;
    use livekit_api::access_token::{AccessToken, VideoGrants};

    type ProbeResult<T> = Result<T, Box<dyn Error>>;

    #[derive(Debug)]
    struct ProbeError(String);

    impl Display for ProbeError {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl Error for ProbeError {}

    fn env_u32(name: &str, fallback: u32) -> u32 {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(fallback)
    }

    fn env_string(name: &str, fallback: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| fallback.into())
    }

    fn make_element(factory: &str) -> ProbeResult<gst::Element> {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|error| ProbeError(format!("create {factory}: {error}")).into())
    }

    fn monitor_pipeline(
        pipeline: &gst::Pipeline,
        parser: &gst::Element,
        duration: Duration,
    ) -> ProbeResult<()> {
        let bus = pipeline
            .bus()
            .ok_or_else(|| ProbeError("pipeline has no bus".into()))?;
        let started = Instant::now();
        let mut reported_caps = false;

        while started.elapsed() < duration {
            if !reported_caps && started.elapsed() >= Duration::from_secs(2) {
                let caps = parser
                    .static_pad("src")
                    .and_then(|pad| pad.current_caps())
                    .map_or_else(|| "unavailable".into(), |caps| caps.to_string());
                println!("[probe] encoded caps: {caps}");
                reported_caps = true;
            }
            let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(250)) else {
                continue;
            };

            match message.view() {
                gst::MessageView::Error(error) => {
                    let source = error.src().map(|source| source.path_string());
                    return Err(ProbeError(format!(
                        "pipeline error from {source:?}: {} ({:?})",
                        error.error(),
                        error.debug()
                    ))
                    .into());
                }
                gst::MessageView::Eos(_) => {
                    return Err(ProbeError("unexpected pipeline EOS".into()).into());
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn run() -> ProbeResult<()> {
        gst::init()?;

        let width = env_u32("PROBE_WIDTH", 1920);
        let height = env_u32("PROBE_HEIGHT", 1080);
        let fps = env_u32("PROBE_FPS", 60);
        let bitrate_kbps = env_u32("PROBE_BITRATE_KBPS", 20_000);
        let duration = Duration::from_secs(u64::from(env_u32("PROBE_DURATION", 20)));
        let room_name = env_string("PROBE_ROOM", "abc-123-xyz");
        let identity = env_string("PROBE_IDENTITY", "gstreamer-probe");
        let ws_url = env_string("PROBE_LIVEKIT_URL", "ws://127.0.0.1:7880");
        let api_key = env_string("LIVEKIT_API_KEY", "devkey");
        let api_secret = env_string("LIVEKIT_API_SECRET", "secret");
        let token = AccessToken::with_api_key(&api_key, &api_secret)
            .with_identity(&identity)
            .with_grants(VideoGrants {
                room_join: true,
                room: room_name.clone(),
                can_publish: true,
                can_subscribe: false,
                can_publish_data: false,
                ..Default::default()
            })
            .to_jwt()?;

        let source = make_element("videotestsrc")?;
        let raw_caps = make_element("capsfilter")?;
        let convert = make_element("videoconvert")?;
        let encoder = make_element("vah264enc")?;
        let parser = make_element("h264parse")?;
        let encoded_caps = make_element("capsfilter")?;
        let queue = make_element("queue")?;
        let sink = make_element("livekitwebrtcsink")?;
        let pipeline = gst::Pipeline::new();

        source.set_property("is-live", true);
        source.set_property_from_str("pattern", "ball");
        raw_caps.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .field("width", width as i32)
                .field("height", height as i32)
                .field("framerate", gst::Fraction::new(fps as i32, 1))
                .build(),
        );
        encoder.set_property("bitrate", bitrate_kbps);
        encoder.set_property("b-frames", 0u32);
        encoder.set_property("cabac", false);
        encoder.set_property("key-int-max", fps.saturating_mul(5).min(1024));
        encoder.set_property("ref-frames", 1u32);
        encoder.set_property_from_str("rate-control", "cbr");
        encoder.set_property_from_str("target-usage", "7");
        parser.set_property("config-interval", -1i32);
        encoded_caps.set_property(
            "caps",
            gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .field("profile", "constrained-baseline")
                .build(),
        );
        queue.set_property("max-size-buffers", 2u32);
        queue.set_property("max-size-bytes", 0u32);
        queue.set_property("max-size-time", 0u64);
        queue.set_property_from_str("leaky", "downstream");
        sink.set_property("video-caps", gst::Caps::builder("video/x-h264").build());
        let sink_proxy = sink
            .dynamic_cast_ref::<gst::ChildProxy>()
            .ok_or_else(|| ProbeError("livekitwebrtcsink does not implement ChildProxy".into()))?;
        sink_proxy.set_child_property("signaller::ws-url", ws_url);
        sink_proxy.set_child_property("signaller::auth-token", token);
        sink_proxy.set_child_property("signaller::room-name", room_name);
        sink_proxy.set_child_property("signaller::identity", identity);
        sink_proxy.set_child_property("signaller::participant-name", "GStreamer probe");

        pipeline.add_many([
            &source,
            &raw_caps,
            &convert,
            &encoder,
            &parser,
            &encoded_caps,
            &queue,
            &sink,
        ])?;
        gst::Element::link_many([
            &source,
            &raw_caps,
            &convert,
            &encoder,
            &parser,
            &encoded_caps,
            &queue,
            &sink,
        ])?;

        pipeline.set_state(gst::State::Playing)?;
        println!(
            "[probe] publishing {width}x{height}@{fps} H.264 at {bitrate_kbps} kbps for {}s",
            duration.as_secs()
        );

        let monitor_result = monitor_pipeline(&pipeline, &parser, duration);
        let stop_result = pipeline.set_state(gst::State::Null);
        monitor_result?;
        stop_result?;
        println!("[probe] completed without a pipeline error");

        Ok(())
    }

    pub(super) fn main() {
        if let Err(error) = run() {
            eprintln!("[probe] failed: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("livekit_gstreamer_probe is only available on Linux");
}
