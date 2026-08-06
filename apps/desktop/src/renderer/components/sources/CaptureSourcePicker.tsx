import { AppWindow, Monitor } from 'lucide-react';
import React, { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { desktopApi } from '../../api/desktop';
import type { CaptureSourceInfo, CaptureSourceSelection } from '../../types';

// In-app source picker for Windows WGC capture. Windows has no system
// picker (unlike the Linux portal dialog), so the renderer lists the screens
// and windows reported by `get_capture_sources` and the user picks one; the
// selection drives the pre-roll capture. Rendered inside the Screenshare
// Source card while `pickerOpen` is set.
interface CaptureSourcePickerProps {
  onSelect: (selection: CaptureSourceSelection) => void;
  onCancel: () => void;
}

export const CaptureSourcePicker: React.FC<CaptureSourcePickerProps> = React.memo(({ onSelect, onCancel }) => {
  const [sources, setSources] = useState<CaptureSourceInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<CaptureSourceSelection | null>(null);

  useEffect(() => {
    let disposed = false;
    void desktopApi.getCaptureSources().then((list) => {
      if (disposed) return;
      if (list.length === 0) {
        setError('No capture sources found — open a window or connect a display, then retry.');
        return;
      }
      setSources(list);
    });
    return () => {
      disposed = true;
    };
  }, []);

  const screens = sources?.filter((source) => source.kind === 'screen') ?? [];
  const windows = sources?.filter((source) => source.kind === 'window') ?? [];

  const isSelected = (source: CaptureSourceInfo): boolean =>
    selected !== null && selected.kind === source.kind && selected.id === source.id;

  const screenLabel = (source: CaptureSourceInfo): string => source.title || `Display ${source.displayId}`;

  let content: React.ReactNode = null;
  if (error !== null) {
    content = <p className="text-sm text-muted-foreground leading-relaxed">{error}</p>;
  } else if (sources === null) {
    content = <p className="text-sm text-muted-foreground">Loading sources…</p>;
  } else {
    content = (
      <>
        {screens.length > 0 && (
          <div className="space-y-1">
            <p className="text-xs text-caption-text">Screens</p>
            {screens.map((source) => (
              <Button
                key={`screen-${source.id}`}
                type="button"
                variant={isSelected(source) ? 'default' : 'outline'}
                size="sm"
                className="w-full justify-start gap-2 font-normal"
                aria-pressed={isSelected(source)}
                onClick={() => setSelected({ kind: 'screen', id: source.id })}
              >
                <Monitor className="size-4 shrink-0" aria-hidden="true" />
                {screenLabel(source)}
              </Button>
            ))}
          </div>
        )}
        {windows.length > 0 && (
          <div className="space-y-1">
            <p className="text-xs text-caption-text">Windows</p>
            {windows.map((source) => (
              <Button
                key={`window-${source.id}`}
                type="button"
                variant={isSelected(source) ? 'default' : 'outline'}
                size="sm"
                className="w-full justify-start gap-2 font-normal truncate"
                aria-pressed={isSelected(source)}
                onClick={() => setSelected({ kind: 'window', id: source.id })}
              >
                <AppWindow className="size-4 shrink-0" aria-hidden="true" />
                {source.title}
              </Button>
            ))}
          </div>
        )}
        <Button
          variant="default"
          className="w-full font-bold"
          disabled={selected === null}
          onClick={() => {
            if (selected !== null) onSelect(selected);
          }}
        >
          Share
        </Button>
      </>
    );
  }

  return (
    <div className="space-y-3 border-t border-border pt-3">
      <div className="flex items-center justify-between">
        <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Choose what to share</p>
        <Button variant="ghost" size="sm" onClick={onCancel} className="h-7 px-2">
          Cancel
        </Button>
      </div>
      {content}
    </div>
  );
});

CaptureSourcePicker.displayName = 'CaptureSourcePicker';
