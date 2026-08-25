# Slopcast

A room-based screen and audio sharing system: a Tauri desktop presenter, a web spectator client, and a Node signaling server. Linux publication flows through a GStreamer pipeline into `livekitwebrtcsink`; Windows flows through the bundled libwebrtc stack.

## Language

**API endpoint**:
The HTTP or HTTPS base URL of the Slopcast server that the desktop uses to check health and manage rooms.
_Avoid_: server URL, API URL

**Endpoint candidate**:
A validated API endpoint waiting to replace the current endpoint after an existing room closes.
_Avoid_: pending endpoint, edited endpoint

**Room endpoint**:
The API endpoint that allocated and manages a specific active room for that room's lifetime.
_Avoid_: active endpoint, current server

**Room replacement**:
The transition that prepares a room on an endpoint candidate, closes the existing room, and connects the presenter to the new room.
_Avoid_: restart stream, switch server

**Active room**:
An ephemeral room registration that may issue participant credentials until it is closed, expires, or its server restarts.
_Avoid_: allocated code, open room

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

**Encoder plan**:
The probe-gated result for a codec: its selected encoder chain and rate-control behavior.
_Avoid_: selected encoder, encoder configuration

**Frame delivery**:
The cross-platform behavior that turns captured frames into publication frames and renderer previews at the active delivery target.
_Avoid_: frame processing, capture pipeline

**Delivery target**:
The width, height, frame rate, and live state that govern frame delivery.
_Avoid_: publication target, stream target, scale target

**Publisher session**:
The lifetime of one presenter publication connection through dormant, connected, recovery, and shutdown states.
_Avoid_: publisher worker, lifecycle loop

**Room closure**:
The irreversible end of a room that disconnects its presenter and spectators and invalidates its room link.
_Avoid_: room disconnect, stop sharing, end stream

**Replay owner**:
The one browser tab allowed to retain a viewer-session buffer for a Slopcast origin. Ownership begins when an eligible tab starts buffering and continues while its live or ended buffer remains available, excluding every other tab until release.
_Avoid_: active replay tab, primary tab, tab leader

**Viewer-session buffer**:
A spectator-local, temporary history of one share interval retained by the replay owner. It remains after sharing stops or the presenter leaves, but disappears when a new share begins or the spectator refreshes or leaves.
_Avoid_: room DVR, recording, archive

**Rewind window**:
The spectator-selected maximum duration retained in a viewer-session buffer. Its default is two minutes.
_Avoid_: retention period, recording length

**Share interval**:
One continuous video publication from when a presenter starts sharing until they stop. A room may contain several share intervals.
_Avoid_: stream, session

**Live edge**:
The newest playable moment in an active share interval. A spectator viewing an older moment is behind live.
_Avoid_: current time, stream end
