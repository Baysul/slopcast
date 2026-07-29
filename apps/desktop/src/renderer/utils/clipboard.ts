export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    if (window.electronAPI?.clipboardWriteText) {
      return await window.electronAPI.clipboardWriteText(text);
    }
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    console.error('copyText failed:', err);
    return false;
  }
}
