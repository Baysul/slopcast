const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('electronAPI', {
  getAppConfig: () => ipcRenderer.invoke('get-app-config'),
  getPlatformInfo: () => ipcRenderer.invoke('get-platform-info'),
  getAudioApps: () => ipcRenderer.invoke('get-audio-apps'),
  dumpAudioSources: () => ipcRenderer.invoke('dump-audio-sources'),
  startAudioCapture: (targetId) => ipcRenderer.invoke('start-audio-capture', targetId),
  stopAudioCapture: () => ipcRenderer.invoke('stop-audio-capture'),
  switchAudioCapture: (targetId) => ipcRenderer.invoke('switch-audio-capture', targetId),
  startAudioMetering: () => ipcRenderer.invoke('start-audio-metering'),
  stopAudioMetering: () => ipcRenderer.invoke('stop-audio-metering'),
  getDesktopSources: () => ipcRenderer.invoke('get-desktop-sources'),
  clipboardWriteText: (text) => ipcRenderer.invoke('clipboard-write-text', text),
  resolveAudioSource: (opts) => ipcRenderer.invoke('resolve-audio-source', opts),
  getCaptureContext: () => ipcRenderer.invoke('get-capture-context'),
  inspectCaptureContext: () => ipcRenderer.invoke('inspect-capture-context'),
  getStreamSettings: () => ipcRenderer.invoke('get-stream-settings'),
  saveStreamSettings: (settings) => ipcRenderer.invoke('save-stream-settings', settings),
  getOnboardingCompleted: () => ipcRenderer.invoke('get-onboarding-completed'),
  setOnboardingCompleted: () => ipcRenderer.invoke('set-onboarding-completed'),
  // Native capture pipeline (native-livekit WASAPI/native-video delivery, in
  // progress): kept for the Task-4 integration, not yet called by the renderer.
  connectNativeRoom: (livekitUrl, token) => ipcRenderer.invoke('connect-native-room', livekitUrl, token),
  disconnectNativeRoom: () => ipcRenderer.invoke('disconnect-native-room'),
  startNativeCapture: (sourceIndex, config) => ipcRenderer.invoke('start-native-capture', sourceIndex, config),
  stopNativeCapture: () => ipcRenderer.invoke('stop-native-capture'),
  stopVideoCapture: () => ipcRenderer.invoke('stop-video-capture'),
  isNativeCaptureActive: () => ipcRenderer.invoke('is-native-capture-active'),
  getSpectatorCount: () => ipcRenderer.invoke('get-spectator-count'),
  onAudioPcmData: (callback) => {
    const handler = (_event, buffer) => callback(buffer);
    ipcRenderer.on('audio-pcm-data', handler);
    return () => ipcRenderer.removeListener('audio-pcm-data', handler);
  },
  onAudioWave: (callback) => {
    const handler = (_event, waves) => callback(waves);
    ipcRenderer.on('audio-wave-update', handler);
    return () => ipcRenderer.removeListener('audio-wave-update', handler);
  },
});
