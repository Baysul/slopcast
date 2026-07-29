export interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  mediaName: string | null;
  sourceType: 'monitor' | 'window' | 'unknown';
  videoNodeCount: number;
  screencastNodeId?: number | null;
}

export interface DesktopSource {
  id: string;
  name: string;
  thumbnail: string;
}
