import type { AudioApp } from '@slopcast/shared-types';

/**
 * The app sentinel for an explicit "no audio" choice. Id 0 never crosses the
 * Tauri IPC boundary as a capture target: selecting it stops capture instead.
 */
export const NO_AUDIO_APP: AudioApp = { id: 0, name: 'No Audio', processId: 0 };

/**
 * Encodes a picker app as a capture target id.
 *
 * On Linux, app audio streams belong to short-lived PipeWire stream nodes
 * whose ids die with the node (a paused YouTube video destroys its node). The
 * process id survives node churn, so active stream entries are encoded as
 * `-processId` (idle entries already use that encoding; system audio stays
 * `-1`). On Windows `AudioApp.id` is already the process id.
 */
export const toAudioTargetId = (app: AudioApp, pidBasedTargets: boolean): number =>
  pidBasedTargets && app.processId > 0 ? -app.processId : app.id;

/**
 * The process id a stored selection targets, when it is a process identity
 * (`-pid` encodings). `-1` (system audio) and `0` (no audio) are not pids.
 */
export const selectedTargetProcessId = (selectedAudioAppId: number | null): number | null =>
  selectedAudioAppId != null && selectedAudioAppId < -1 ? -selectedAudioAppId : null;

/**
 * True when the selected capture target is a negative pid whose process owns
 * an app in `members`. Falls back to an exact id match for targets that are
 * node ids (pid 0 entries) or the -1 system-audio sentinel.
 */
export const isGroupSelected = (members: AudioApp[], selectedAudioAppId: number | null): boolean => {
  const targetProcessId = selectedTargetProcessId(selectedAudioAppId);
  if (targetProcessId !== null) {
    return members.some((m) => m.processId === targetProcessId);
  }
  return selectedAudioAppId != null && members.some((m) => m.id === selectedAudioAppId);
};
