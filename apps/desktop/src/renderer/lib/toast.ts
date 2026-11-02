import { toast as sonnerToast } from 'sonner';

import { playNotificationSound } from '../hooks/useNotificationSound';
import type { NotificationVariant } from '../hooks/useNotificationSound';

export type { NotificationVariant } from '../hooks/useNotificationSound';
export { primeAudioContext } from '../hooks/useNotificationSound';

export const notify = (variant: NotificationVariant, title: string, description?: string) => {
  playNotificationSound(variant);
  sonnerToast[variant](title, { description });
};
