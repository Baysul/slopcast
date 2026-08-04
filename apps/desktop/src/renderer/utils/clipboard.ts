import { writeText } from '@tauri-apps/plugin-clipboard-manager';

export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    await writeText(text);
    return true;
  } catch (err) {
    console.error('clipboard writeText failed:', err);
  }
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    console.error('copyText failed:', err);
    return false;
  }
}
