interface NoticeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const SPECTATOR_CAPABILITY_NOTICE_KEY = 'slopcast:spectator-capability-notice-seen';

export const shouldShowSpectatorCapabilityNotice = (storage: NoticeStorage): boolean => {
  try {
    return storage.getItem(SPECTATOR_CAPABILITY_NOTICE_KEY) !== 'true';
  } catch (error) {
    console.info('[SpectatorBanner] Saved notice state is unavailable:', error);
    return true;
  }
};

export const markSpectatorCapabilityNoticeSeen = (storage: NoticeStorage): void => {
  try {
    storage.setItem(SPECTATOR_CAPABILITY_NOTICE_KEY, 'true');
  } catch (error) {
    console.info('[SpectatorBanner] Notice state could not be saved:', error);
  }
};
