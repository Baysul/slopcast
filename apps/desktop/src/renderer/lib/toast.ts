import { toast as sonnerToast } from 'sonner';
import type { NotificationVariant } from '../hooks/useNotificationSound';
import { playNotificationSound } from '../hooks/useNotificationSound';

export type { NotificationVariant } from '../hooks/useNotificationSound';
export { primeAudioContext } from '../hooks/useNotificationSound';

export function notify(variant: NotificationVariant, title: string, description?: string) {
  playNotificationSound(variant);
  sonnerToast[variant](title, { description });
}
