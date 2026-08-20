# Attempt unverified NVENC bitrate re-targeting

Status: accepted

The congestion controller re-targets the encoder's bitrate about once per second, and the NVENC `bitrate` property is runtime-settable but whether the driver honors mid-stream CBR changes is unverified. We attempt the re-target, bookkeep it as applied, and log a one-time warning — because the failure mode (the controller believes the cap moved while the encoder holds the old rate) is bounded by the configured ceiling and degrades to exactly the pinned behavior we rejected (the `av1enc` path), and no NVIDIA hardware exists in development to verify it.
