# Slopcast

A room-based screen and audio sharing system: a Tauri desktop presenter, a web spectator client, and a Node signaling server. Linux publication flows through a GStreamer pipeline into `livekitwebrtcsink`; Windows flows through the bundled libwebrtc stack.

## Language

**Encoder chain**:
The ordered, probe-gated preference of video encoders for a codec: NVENC → VA-API → software.
_Avoid_: encoder preference list, codec fallback stack

**Probe gate**:
The `can_initialize_element` check that selects the first encoder in the chain that actually initializes on this machine.
_Avoid_: capability check, encoder detection

**Branch pre-chain**:
The GStreamer elements upstream of the encoder that prepare frames for it (`videoconvert` for VA/software, `cudaupload ! cudaconvertscale` for NVENC).
_Avoid_: encoder prefix, conversion stage

**Hardware encoder suffix**:
The encoder name shown in the picker next to a hardware codec (e.g. "H.264 (NVENC)").
_Avoid_: codec badge, vendor label
