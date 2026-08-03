import * as nativeLiveKit from '@slopcast/native-livekit';
import * as native from '@slopcast/native-rust';
import { loadConfig } from '@slopcast/shared-types/config';
import { app, clipboard, ipcMain } from 'electron';
import { registerAudioHandlers, registerWaveCallback } from './audio';
import type { MainContext } from './context';
import { getWindow, isWayland } from './context';
import { registerRoomHandlers } from './room';
import { isOnboardingCompleted, loadStreamSettings, saveStreamSettings, setOnboardingCompleted } from './settings';
import {
  registerAudioDataCallback,
  registerDmabufCallback,
  registerVideoHandlers,
  setLastCapturedSourceName,
  stopNativeCapture,
} from './video';
import { createWindow } from './window';

const appConfig = loadConfig();

app.whenReady().then(() => {
  app.setName('slopcast');
  console.log('====================================================');
  console.log('🚀 Launching Desktop Presenter Application');
  console.log(`   Platform: ${process.platform} (${isWayland ? 'Wayland - xdg-desktop-portal' : 'X11/native'})`);
  console.log('====================================================');

  try {
    const initMsg = native.initEngine();
    console.log(`[Native Rust] ${initMsg}`);
  } catch (err) {
    console.error('❌ Native audio engine error:', err);
  }

  const ctx: MainContext = {
    getWindow,
    native,
    nativeLiveKit,
    isWayland,
    registerDmabufCallback,
    registerAudioDataCallback,
    registerWaveCallback,
  };

  // IPC Handlers
  ipcMain.handle('get-app-config', () => ({
    apiEndpoint: appConfig.apiEndpoint,
    livekitUrl: appConfig.livekitUrl,
  }));

  ipcMain.handle('get-stream-settings', () => loadStreamSettings());

  ipcMain.handle('save-stream-settings', (_event, raw: unknown) => saveStreamSettings(raw));

  ipcMain.handle('get-onboarding-completed', () => isOnboardingCompleted());

  ipcMain.handle('set-onboarding-completed', () => setOnboardingCompleted());

  ipcMain.handle('get-platform-info', () => ({
    platform: process.platform,
    isWayland,
  }));

  ipcMain.handle('clipboard-write-text', (_event, text: string) => {
    try {
      clipboard.writeText(String(text ?? ''));
      return true;
    } catch (err) {
      console.error('clipboard-write-text IPC error:', err);
      return false;
    }
  });

  registerVideoHandlers(ctx);
  registerAudioHandlers(ctx);
  registerRoomHandlers(ctx);

  createWindow({
    stopNativeCapture,
    setLastCapturedSourceName,
  });
});

app.on('before-quit', () => {
  stopNativeCapture();
});

app.on('window-all-closed', () => {
  app.quit();
});
