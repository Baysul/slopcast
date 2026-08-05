/// <reference types="vite/client" />

// Preview benchmark hook (Phase 2, set by the wdio bench spec): records
// [pts_us, arrival_ms, draw_ms] per drawn frame. Never touched in normal runs.
interface Window {
  __PREVIEW_BENCH__?: boolean;
  __PREVIEW_BENCH_DATA__?: Array<[number, number, number | null]>;
}
