import { ipcMain } from 'electron';
import type { MainContext } from './context';

export function registerRoomHandlers(ctx: MainContext) {
  ipcMain.handle('connect-native-room', (_event, livekitUrl: string, token: string) => {
    try {
      ctx.nativeLiveKit.connectLivekitRoom(livekitUrl, token);
      console.log(`[native-room] connect requested (url=${livekitUrl})`);
      return true;
    } catch (err) {
      console.error('connect-native-room IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('disconnect-native-room', () => {
    try {
      ctx.nativeLiveKit.disconnectLivekitRoom();
      console.log('[native-room] disconnect requested');
      return true;
    } catch (err) {
      console.error('disconnect-native-room IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('get-spectator-count', () => {
    try {
      return ctx.nativeLiveKit.getSpectatorCount();
    } catch (err) {
      console.error('get-spectator-count IPC error:', err);
      return 0;
    }
  });

  ipcMain.handle('is-native-room-connected', () => {
    try {
      return ctx.nativeLiveKit.isLivekitRoomConnected();
    } catch (err) {
      console.error('is-native-room-connected IPC error:', err);
      return false;
    }
  });
}
