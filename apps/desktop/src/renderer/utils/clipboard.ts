import { writeText } from '@tauri-apps/plugin-clipboard-manager';

const writeWithTauri = async (text: string): Promise<boolean> => {
  try {
    await writeText(text);
    return true;
  } catch (err) {
    console.error('clipboard writeText failed:', err);
    return false;
  }
};

const writeWithNavigator = async (text: string): Promise<boolean> => {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    console.error('copyText failed:', err);
    return false;
  }
};

export const copyText = async (text: string): Promise<boolean> => {
  if (!text) {
    return false;
  }
  if (await writeWithTauri(text)) {
    return true;
  }
  return writeWithNavigator(text);
};
