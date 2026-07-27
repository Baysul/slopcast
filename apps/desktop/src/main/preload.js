const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('electronAPI', {
  getAppConfig: () => ipcRenderer.invoke('get-app-config'),
  getPlatformInfo: () => ipcRenderer.invoke('get-platform-info'),
  getAudioApps: () => ipcRenderer.invoke('get-audio-apps'),
  startAudioCapture: (targetId) => ipcRenderer.invoke('start-audio-capture', targetId),
  stopAudioCapture: () => ipcRenderer.invoke('stop-audio-capture'),
  switchAudioCapture: (targetId) => ipcRenderer.invoke('switch-audio-capture', targetId),
  startAudioMetering: () => ipcRenderer.invoke('start-audio-metering'),
  stopAudioMetering: () => ipcRenderer.invoke('stop-audio-metering'),
  getAudioLevels: () => ipcRenderer.invoke('get-audio-levels'),
  getDesktopSources: () => ipcRenderer.invoke('get-desktop-sources'),
  clipboardWriteText: (text) => ipcRenderer.invoke('clipboard-write-text', text),
  resolveAudioSource: (opts) => ipcRenderer.invoke('resolve-audio-source', opts),
  getCaptureContext: () => ipcRenderer.invoke('get-capture-context'),
  getStreamSettings: () => ipcRenderer.invoke('get-stream-settings'),
  saveStreamSettings: (settings) => ipcRenderer.invoke('save-stream-settings', settings),
});
