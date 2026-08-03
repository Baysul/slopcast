import type { AudioApp, AudioAppWave, StreamSettings } from '@slopcast/shared-types';
import type { CaptureContext } from './index';

declare global {
  interface Window {
    electronAPI?: {
      getAppConfig: () => Promise<{ apiEndpoint: string; livekitUrl: string }>;
      getPlatformInfo: () => Promise<{ platform: string; isWayland: boolean }>;
      getAudioApps: () => Promise<AudioApp[]>;
      dumpAudioSources: () => Promise<Array<Record<string, string>>>;
      startAudioCapture: (targetId: number) => Promise<boolean>;
      stopAudioCapture: () => Promise<boolean>;
      switchAudioCapture: (targetId: number) => Promise<boolean>;
      startAudioMetering: () => Promise<boolean>;
      stopAudioMetering: () => Promise<boolean>;
      getDesktopSources: () => Promise<Array<{ id: string; name: string; thumbnail: string }>>;
      clipboardWriteText: (text: string) => Promise<boolean>;
      resolveAudioSource: (opts?: { sourceId?: string; nameHint?: string }) => Promise<AudioApp | null>;
      getCaptureContext: () => Promise<CaptureContext | null>;
      inspectCaptureContext: () => Promise<{
        de: string;
        sourceType: string;
        mediaName: string | null;
        videoNodeCount: number;
        screencastNodeId: number | null;
        app: AudioApp | null;
        portalProps: Record<string, string> | null;
        windowPid: number | null;
        windowCaption: string | null;
      } | null>;
      getStreamSettings: () => Promise<StreamSettings>;
      saveStreamSettings: (settings: StreamSettings) => Promise<boolean>;
      getOnboardingCompleted: () => Promise<boolean>;
      setOnboardingCompleted: () => Promise<boolean>;
      // Native capture pipeline (native-livekit WASAPI/native-video delivery, in
      // progress): kept for the Task-4 integration, not yet called by the renderer.
      connectNativeRoom: (livekitUrl: string, token: string) => Promise<boolean>;
      disconnectNativeRoom: () => Promise<boolean>;
      startNativeCapture: (
        sourceIndex: number,
        config: { fps: number; width: number; height: number; videoCodec?: string },
      ) => Promise<{ ok: boolean; nodeId: number | null; error?: string }>;
      stopNativeCapture: () => Promise<boolean>;
      stopVideoCapture: () => Promise<boolean>;
      isNativeCaptureActive: () => Promise<boolean>;
      getSpectatorCount: () => Promise<number>;
      onAudioPcmData: (callback: (buffer: ArrayBuffer) => void) => () => void;
      onAudioWave?: (callback: (waves: AudioAppWave[]) => void) => () => void;
    };
  }
}
