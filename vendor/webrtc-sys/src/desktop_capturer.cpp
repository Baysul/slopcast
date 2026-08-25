/*
 * Copyright 2025 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#include "livekit/desktop_capturer.h"

#include <atomic>

#include "modules/desktop_capture/desktop_and_cursor_composer.h"
#include "modules/desktop_capture/desktop_capture_options.h"

using SourceList = webrtc::DesktopCapturer::SourceList;

namespace livekit_ffi {

// Counters for cursor composition observability. The compose decision happens
// inside libwebrtc (DesktopAndCursorComposer may_contain_cursor gate), so the
// only truth we can expose is what the frame reports after composition: whether
// the frame claims to contain a cursor and how many frames were delivered.
// These are process-wide because the Rust side resets them per capture session.
static std::atomic<uint64_t> g_frames_delivered{0};
static std::atomic<uint64_t> g_frames_with_cursor{0};

CursorStats get_cursor_stats() {
  return CursorStats{g_frames_delivered.load(std::memory_order_relaxed),
                     g_frames_with_cursor.load(std::memory_order_relaxed)};
}

void reset_cursor_stats() {
  g_frames_delivered.store(0, std::memory_order_relaxed);
  g_frames_with_cursor.store(0, std::memory_order_relaxed);
}

std::unique_ptr<DesktopCapturer> new_desktop_capturer(
    DesktopCapturerOptions options) {
  webrtc::DesktopCaptureOptions webrtc_options =
      webrtc::DesktopCaptureOptions::CreateDefault();
#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
  webrtc_options.set_allow_sck_capturer(true);
  webrtc_options.set_allow_sck_system_picker(options.allow_sck_system_picker);
#endif /* defined(WEBRTC_MAC) && !defined(WEBRTC_IOS) */
#ifdef _WIN64
  switch (options.source_type) {
    case SourceType::Screen:
      webrtc_options.set_allow_wgc_screen_capturer(true);
      break;
    case SourceType::Window:
      webrtc_options.set_allow_wgc_window_capturer(true);
      // https://github.com/webrtc-sdk/webrtc/blob/m137_release/modules/desktop_capture/desktop_capture_options.h#L133-L142
      webrtc_options.set_enumerate_current_process_windows(false);
      break;
    default:
      break;
  }
  webrtc_options.set_allow_directx_capturer(true);
#endif /* _WIN64 */
#ifdef WEBRTC_USE_PIPEWIRE
  webrtc_options.set_allow_pipewire(true);
#endif /* WEBRTC_USE_PIPEWIRE */

  // prefer_cursor_embedded asks the OS capturer to paint the cursor into the
  // frame pixels. WGC honors it and paints the cursor natively. The PipeWire
  // capturer in m144 must NOT get this flag: BaseCapturerPipeWire forwards it
  // into SharedScreenCastStream, which then marks every frame
  // may_contain_cursor=true, while ScreenCastPortal ignores it and still
  // negotiates cursor_mode=metadata with the compositor. The frame claims to
  // contain a cursor it does not contain, so DesktopAndCursorComposer skips
  // the alpha-blend of the SPA_META_Cursor metadata. Keeping it false on the
  // PipeWire arm lets the composer do the blend instead.
  bool prefer_embedded = options.include_cursor;
#if defined(WEBRTC_USE_PIPEWIRE) && !defined(WEBRTC_MAC) && !defined(_WIN64)
  prefer_embedded = false;
#endif
  webrtc_options.set_prefer_cursor_embedded(prefer_embedded);

  std::unique_ptr<webrtc::DesktopCapturer> capturer = nullptr;
  switch (options.source_type) {
    case SourceType::Window:
      capturer = webrtc::DesktopCapturer::CreateWindowCapturer(webrtc_options);
      break;
    case SourceType::Screen:
      capturer = webrtc::DesktopCapturer::CreateScreenCapturer(webrtc_options);
      break;
    case SourceType::Generic:
      capturer = webrtc::DesktopCapturer::CreateGenericCapturer(webrtc_options);
      break;
    default:
      return nullptr;
  }

  if (!capturer) {
    return nullptr;
  }

  // The PipeWire capturer (Wayland portal) never embeds the cursor into the
  // frame pixels: `BaseCapturerPipeWire` requests `cursor_mode=metadata` from
  // xdg-desktop-portal (the `prefer_cursor_embedded` option is ignored in
  // m144 — upstream fixed this later in crrev.com/c/92db6816), so the
  // compositor attaches the cursor as `SPA_META_Cursor` stream metadata
  // instead of rendering it. Wrap the capturer in a `DesktopAndCursorComposer`
  // so the cursor shape/position (read from the same `SharedScreenCastStream`
  // via `MouseCursorMonitorPipeWire`) is alpha-blended into every frame — the
  // same path Chromium uses for Wayland screensharing.
  //
  // Windows WGC already paints the cursor into the frames
  // (`prefer_cursor_embedded` is honored there), so the composer is only
  // needed on the PipeWire arm.
#if defined(WEBRTC_USE_PIPEWIRE) && !defined(WEBRTC_MAC) && !defined(_WIN64)
  if (options.include_cursor && webrtc_options.allow_pipewire()) {
    capturer = std::make_unique<webrtc::DesktopAndCursorComposer>(
        std::move(capturer), webrtc_options);
  }
#endif /* WEBRTC_USE_PIPEWIRE && !WEBRTC_MAC && !_WIN64 */

  return std::make_unique<DesktopCapturer>(std::move(capturer));
}

void DesktopCapturer::start(
    rust::Box<DesktopCapturerCallbackWrapper> callback) {
  this->callback = std::move(callback);
  capturer->Start(this);
}

void DesktopCapturer::OnCaptureResult(
    webrtc::DesktopCapturer::Result result,
    std::unique_ptr<webrtc::DesktopFrame> frame) {
  if (result == webrtc::DesktopCapturer::Result::SUCCESS && frame) {
    g_frames_delivered.fetch_add(1, std::memory_order_relaxed);
    if (frame->may_contain_cursor()) {
      g_frames_with_cursor.fetch_add(1, std::memory_order_relaxed);
    }
  }

  CaptureResult ret_result = CaptureResult::ErrorPermanent;
  switch (result) {
    case webrtc::DesktopCapturer::Result::SUCCESS:
      ret_result = CaptureResult::Success;
      break;
    case webrtc::DesktopCapturer::Result::ERROR_PERMANENT:
      ret_result = CaptureResult::ErrorPermanent;
      break;
    case webrtc::DesktopCapturer::Result::ERROR_TEMPORARY:
      ret_result = CaptureResult::ErrorTemporary;
      break;
    default:
      break;
  }
  if (callback) {
    (*callback)->on_capture_result(
        ret_result, std::make_unique<DesktopFrame>(std::move(frame)));
  }
}

rust::Vec<Source> DesktopCapturer::get_source_list() const {
  SourceList list{};
  bool res = capturer->GetSourceList(&list);
  rust::Vec<Source> source_list{};
  if (res) {
    for (auto& source : list) {
      source_list.push_back(Source{static_cast<uint64_t>(source.id),
                                   source.title, source.display_id});
    }
  }
  return source_list;
}
}  // namespace livekit_ffi