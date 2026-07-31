const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('electronAPI', {
  getAppConfig: () => ipcRenderer.invoke('get-app-config'),
  getPlatformInfo: () => ipcRenderer.invoke('get-platform-info'),
  getAudioApps: () => ipcRenderer.invoke('get-audio-apps'),
  startAudioCapture: (targetId) => ipcRenderer.invoke('start-audio-capture', targetId),
  stopAudioCapture: () => ipcRenderer.invoke('stop-audio-capture'),
  connectNativeRoom: (livekitUrl, token) => ipcRenderer.invoke('connect-native-room', livekitUrl, token),
  disconnectNativeRoom: () => ipcRenderer.invoke('disconnect-native-room'),
  switchAudioCapture: (targetId) => ipcRenderer.invoke('switch-audio-capture', targetId),
  startAudioMetering: () => ipcRenderer.invoke('start-audio-metering'),
  stopAudioMetering: () => ipcRenderer.invoke('stop-audio-metering'),
  getAudioLevels: () => ipcRenderer.invoke('get-audio-levels'),
  getDesktopSources: () => ipcRenderer.invoke('get-desktop-sources'),
  clipboardWriteText: (text) => ipcRenderer.invoke('clipboard-write-text', text),
  resolveAudioSource: (opts) => ipcRenderer.invoke('resolve-audio-source', opts),
  resolveAudioAppByName: (label) => ipcRenderer.invoke('resolve-audio-app-by-name', label),
  getCaptureContext: () => ipcRenderer.invoke('get-capture-context'),
  getStreamSettings: () => ipcRenderer.invoke('get-stream-settings'),
  saveStreamSettings: (settings) => ipcRenderer.invoke('save-stream-settings', settings),
  getOnboardingCompleted: () => ipcRenderer.invoke('get-onboarding-completed'),
  setOnboardingCompleted: () => ipcRenderer.invoke('set-onboarding-completed'),
  listScreenSources: () => ipcRenderer.invoke('list-screen-sources'),
  startNativeCapture: (sourceIndex, config) => ipcRenderer.invoke('start-native-capture', sourceIndex, config),
  startVideoCapture: (nodeId, width, height, fps) =>
    ipcRenderer.invoke('start-video-capture', nodeId, width, height, fps),
  stopVideoCapture: () => ipcRenderer.invoke('stop-video-capture'),
  stopNativeCapture: () => ipcRenderer.invoke('stop-native-capture'),
  isNativeCaptureActive: () => ipcRenderer.invoke('is-native-capture-active'),
  getSpectatorCount: () => ipcRenderer.invoke('get-spectator-count'),
  selectVideoFile: () => ipcRenderer.invoke('select-video-file'),
  probeVideoFile: (filePath) => ipcRenderer.invoke('probe-video-file', filePath),
  startVideoFile: (filePath) => ipcRenderer.invoke('start-video-file', filePath),
  stopVideoFile: () => ipcRenderer.invoke('stop-video-file'),
  seekVideoFile: (tsMs) => ipcRenderer.invoke('seek-video-file', tsMs),
  pauseVideoFile: (paused) => ipcRenderer.invoke('pause-video-file', paused),
  onVideoFileFrame: (cb) => {
    const handler = (_e, data) => cb(data);
    ipcRenderer.on('video:frame', handler);
    return () => ipcRenderer.removeListener('video:frame', handler);
  },
  onVideoFileAudio: (cb) => {
    const handler = (_e, data) => cb(data);
    ipcRenderer.on('video:audio', handler);
    return () => ipcRenderer.removeListener('video:audio', handler);
  },
});
