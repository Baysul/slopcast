# Keep spectator replay in the browser

Status: accepted

Spectator rewind uses a per-tab viewer-session buffer backed by IndexedDB instead of LiveKit Egress or server-side DVR. This preserves ephemeral rooms and avoids adding media storage, authorization, and retention infrastructure, at the cost of browser-dependent replay support and spectator-side CPU and storage use.
