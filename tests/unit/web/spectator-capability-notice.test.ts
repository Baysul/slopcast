import assert from 'node:assert/strict';
import test from 'node:test';
import {
  markSpectatorCapabilityNoticeSeen,
  shouldShowSpectatorCapabilityNotice,
} from '../../../apps/web/src/components/spectator-capability-notice.ts';

class MemoryStorage {
  private readonly values = new Map<string, string>();

  public getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  public setItem(key: string, value: string): void {
    this.values.set(key, value);
  }
}

test('the spectator capability notice only appears on the first visit', () => {
  const storage = new MemoryStorage();

  assert.equal(shouldShowSpectatorCapabilityNotice(storage), true);
  markSpectatorCapabilityNoticeSeen(storage);
  assert.equal(shouldShowSpectatorCapabilityNotice(storage), false);
});
