//! Codec availability and recommendation, ported from `utils/codecs.ts`.

use crate::settings::VALID_CODECS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecInfo {
    pub codec: String,
    pub label: String,
    pub hardware: bool,
    pub recommended: bool,
}

/// The renderer's preference order: h264 > h265 > vp8 > vp9 > av1.
const PREFERENCE: [&str; 5] = ["h264", "h265", "vp8", "vp9", "av1"];

fn priority(codec: &str) -> usize {
    PREFERENCE.iter().position(|c| *c == codec).unwrap_or(99)
}

#[must_use]
pub fn sort_by_codec_preference(codecs: &[CodecInfo]) -> Vec<CodecInfo> {
    let mut sorted = codecs.to_vec();
    sorted.sort_by_key(|c| priority(&c.codec));
    sorted
}

#[must_use]
pub fn from_native(infos: &[native_livekit::NativeCodecInfo]) -> Vec<CodecInfo> {
    let codecs = infos
        .iter()
        .filter(|info| VALID_CODECS.contains(&info.codec.as_str()))
        .map(|info| CodecInfo {
            codec: info.codec.clone(),
            label: info.label.clone(),
            hardware: info.hardware,
            recommended: false,
        })
        .collect::<Vec<_>>();
    sort_by_codec_preference(&codecs)
}

/// Moves the recommended codec first: hardware H.264, else the shipped VP8 default, else the first.
#[must_use]
pub fn recommend_codec(codecs: &[CodecInfo]) -> Vec<CodecInfo> {
    let Some(recommended) = codecs
        .iter()
        .find(|c| c.hardware && c.codec == "h264")
        .or_else(|| codecs.iter().find(|c| c.codec == "vp8"))
        .or_else(|| codecs.first())
    else {
        return Vec::new();
    };
    let mut result = vec![CodecInfo {
        recommended: true,
        ..recommended.clone()
    }];
    result.extend(
        codecs
            .iter()
            .filter(|c| c.codec != recommended.codec)
            .cloned(),
    );
    result
}

/// Human label for a codec mime type such as `video/VP8` or a bare id.
#[must_use]
pub fn codec_label(mime: &str) -> String {
    let id = mime.rsplit('/').next().unwrap_or(mime).to_ascii_lowercase();
    match id.as_str() {
        "h264" => "H.264".into(),
        "h265" => "H.265".into(),
        "vp8" => "VP8".into(),
        "vp9" => "VP9".into(),
        "av1" => "AV1".into(),
        "opus" => "Opus".into(),
        _ => mime.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: &str, hardware: bool) -> CodecInfo {
        CodecInfo {
            codec: codec.into(),
            label: codec.to_uppercase(),
            hardware,
            recommended: false,
        }
    }

    fn ids(codecs: &[CodecInfo]) -> Vec<&str> {
        codecs.iter().map(|c| c.codec.as_str()).collect()
    }

    #[test]
    fn sorts_by_preference() {
        let sorted = sort_by_codec_preference(&[
            info("av1", false),
            info("h265", false),
            info("h264", false),
            info("vp9", false),
            info("vp8", false),
        ]);
        assert_eq!(ids(&sorted), ["h264", "h265", "vp8", "vp9", "av1"]);
    }

    #[test]
    fn drops_unknown_native_codecs() {
        let native = [
            native_livekit::NativeCodecInfo {
                codec: "h265".into(),
                label: "H.265".into(),
                hardware: true,
            },
            native_livekit::NativeCodecInfo {
                codec: "theora".into(),
                label: "Theora".into(),
                hardware: false,
            },
        ];
        assert_eq!(ids(&from_native(&native)), ["h265"]);
    }

    #[test]
    fn recommendation_order() {
        let software =
            recommend_codec(&[info("h264", false), info("vp8", false), info("av1", false)]);
        assert_eq!(software[0].codec, "vp8");
        assert!(software[0].recommended);
        assert!(software[1..].iter().all(|c| !c.recommended));
        let hardware = recommend_codec(&[info("h264", true), info("vp8", false)]);
        assert_eq!(hardware[0].codec, "h264");
        let fallback = recommend_codec(&[info("h264", false), info("vp9", false)]);
        assert_eq!(fallback[0].codec, "h264");
        assert!(recommend_codec(&[]).is_empty());
    }

    #[test]
    fn labels_mime_types() {
        assert_eq!(codec_label("video/VP8"), "VP8");
        assert_eq!(codec_label("audio/opus"), "Opus");
        assert_eq!(codec_label("video/x-custom"), "video/x-custom");
    }
}
