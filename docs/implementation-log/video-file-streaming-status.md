# Video File Streaming — Implementation Status

Plan: `docs/architecture-research/video-file-streaming.md`
Phase: 1 — Browser-native MVP (`<video>` + `captureStream()`)

## Task Graph

| Task | Phase | Status | Files | Reviewer verdict | Rework rounds |
|---|---|---|---|---|---|
| 1 — shared types | 1 | verified | `packages/shared-types/src/index.ts` | APPROVE | 0 |
| 2 — main process: file dialog, protocol, preload, persistence | 1 | verified | `apps/desktop/src/main/index.ts`, `apps/desktop/src/renderer/index.html`, `apps/desktop/src/renderer/types/electron-api.d.ts` | APPROVE | 2 |
| 3 — renderer: source type, file capture, publish, UI, EOF, controls | 1 | verified | `apps/desktop/src/renderer/main.tsx` | APPROVE | 1 |

### Dependencies
- Task 2 depends on Task 1 (needs `VideoSourceType`, `VideoFileSourceConfig`, `StreamSettings` shape)
- Task 3 depends on Task 1 and Task 2
