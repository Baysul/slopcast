import { FolderOpen, Keyboard, Palette, Settings2, Video } from 'lucide-react';
import type { KeyboardEvent, ReactNode } from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { desktopApi } from '@/api/desktop';
import { windowControls } from '@/api/windowControls';
import { TitleBar } from '@/components/layout/TitleBar';
import { WindowResizeHandles } from '@/components/layout/WindowResizeHandles';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { useDesktopPreferences } from '@/hooks/useDesktopPreferences';
import type { ShortcutAction, ShortcutBindings, ShortcutReport } from '@/types/preferences';
import { formatShortcut, SHORTCUT_ACTIONS } from '@/types/preferences';
import { AppearanceSettings } from './AppearanceSettings';
import { StreamSettingsPanel } from './StreamSettingsPanel';

type EditableShortcut = ShortcutAction | 'toggle-telemetry';

const SETTINGS_SECTIONS = [
  { id: 'general', label: 'General', icon: Settings2 },
  { id: 'appearance', label: 'Appearance', icon: Palette },
  { id: 'stream', label: 'Stream', icon: Video },
  { id: 'shortcuts', label: 'Shortcuts', icon: Keyboard },
] as const;

function readChord(event: KeyboardEvent<HTMLButtonElement>, isGlobal: boolean): string | null {
  if (!event.ctrlKey && !event.metaKey) return null;
  if (isGlobal && !event.shiftKey && !event.altKey) return null;

  const parts: string[] = [];
  if (event.ctrlKey) parts.push('Control');
  if (event.metaKey) parts.push('Command');
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');
  parts.push(event.code);

  return parts.join('+');
}

function PreferencesLoadState({ error, refresh }: { error: string | null; refresh: () => Promise<void> }) {
  if (!error)
    return (
      <p className="text-sm text-muted-foreground" role="status">
        Loading preferences…
      </p>
    );

  return (
    <div className="space-y-3">
      <p className="text-sm text-destructive" role="alert">
        Could not load preferences: {error}
      </p>
      <Button variant="outline" onClick={() => void refresh()}>
        Retry
      </Button>
    </div>
  );
}

function ShortcutStatus({ report }: { report: ShortcutReport | null }) {
  if (report && !report.status) return null;

  return (
    <div className="rounded-md border border-border bg-secondary/40 p-3 text-sm" role="status">
      <p>{report?.status ?? 'Connecting to the desktop shortcut service…'}</p>
    </div>
  );
}

export function DesktopSettings() {
  const { snapshot, setSnapshot, error: loadError, refresh } = useDesktopPreferences();
  const [section, setSection] = useState<(typeof SETTINGS_SECTIONS)[number]['id']>('general');
  const [report, setReport] = useState<ShortcutReport | null>(null);
  const [draft, setDraft] = useState<ShortcutBindings | null>(null);
  const [editing, setEditing] = useState<EditableShortcut | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const busyRef = useRef<boolean>(false);
  const recordButtonRef = useRef<HTMLButtonElement>(null);
  const lastEditingRef = useRef<EditableShortcut | null>(null);
  const pathInputRef = useRef<HTMLInputElement>(null);
  const savedBindingsRef = useRef<ShortcutBindings | null>(null);

  const perform = useCallback(async (operation: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    setMessage('');
    try {
      await operation();
    } catch (cause) {
      setError(String(cause));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    if (!snapshot) return;
    const shortcuts = snapshot.preferences.shortcuts;
    if (
      savedBindingsRef.current &&
      SHORTCUT_ACTIONS.every((action) => savedBindingsRef.current?.[action.id] === shortcuts[action.id])
    )
      return;
    savedBindingsRef.current = shortcuts;
    setDraft(shortcuts);
  }, [snapshot]);

  useEffect(() => {
    const unlisten = desktopApi.onShortcutsChanged(setReport);
    const unlistenClose = desktopApi.onSettingsCloseRequested(() => {
      setEditing(null);
      setMessage('');
    });
    void desktopApi
      .getShortcutStatus()
      .then(setReport)
      .catch((cause: unknown) => setError(String(cause)));
    return () => {
      void unlisten.then((stop) => stop());
      void unlistenClose.then((stop) => stop());
      void desktopApi
        .setShortcutRecording(false)
        .catch((cause: unknown) => console.error('Cannot end shortcut recording:', cause));
    };
  }, []);

  useEffect(() => {
    if (editing) {
      lastEditingRef.current = editing;
      recordButtonRef.current?.focus();
    } else if (lastEditingRef.current) {
      document.querySelector<HTMLButtonElement>(`[data-shortcut-action="${lastEditingRef.current}"]`)?.focus();
      lastEditingRef.current = null;
    }
  }, [editing]);

  const endRecording = useCallback(async () => {
    await desktopApi.setShortcutRecording(false);
    setEditing(null);
  }, []);

  useEffect(() => {
    if (!editing) return;
    const cancel = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      void perform(endRecording);
    };
    window.addEventListener('keydown', cancel, true);
    return () => window.removeEventListener('keydown', cancel, true);
  }, [editing, endRecording, perform]);

  useEffect(() => {
    if (editing) return;
    const closeOnEscape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      event.preventDefault();
      void windowControls.close();
    };
    window.addEventListener('keydown', closeOnEscape);
    return () => window.removeEventListener('keydown', closeOnEscape);
  }, [editing]);

  const saveRecordedGlobalShortcut = (action: ShortcutAction, chord: string) => {
    if (chord === snapshot?.preferences.telemetryShortcut) {
      setError('That shortcut is already assigned to telemetry.');
      return;
    }
    if (draft && SHORTCUT_ACTIONS.some((row) => row.id !== action && draft[row.id] === chord)) {
      setError('That shortcut is already assigned to another action.');
      return;
    }
    void perform(async () => {
      await endRecording();
      setDraft((current) => current && { ...current, [action]: chord });
      setMessage('Shortcut recorded. Apply shortcuts to activate it.');
    });
  };

  const recordShortcut = (event: KeyboardEvent<HTMLButtonElement>, action: EditableShortcut) => {
    if (event.key === 'Tab') return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === 'Escape') {
      void perform(endRecording);
      return;
    }
    if (event.repeat || event.nativeEvent.isComposing || ['Control', 'Shift', 'Alt', 'Meta'].includes(event.key))
      return;
    const isGlobal = action !== 'toggle-telemetry';
    const chord = readChord(event, isGlobal);
    if (!chord) {
      let hint = 'Include Ctrl or Cmd.';
      if (isGlobal) hint = 'Include Ctrl or Cmd, plus Shift or Alt. Editing and desktop-reserved keys stay available.';
      setError(hint);
      return;
    }
    if (!isGlobal) {
      void perform(async () => {
        await endRecording();
        setSnapshot(await desktopApi.saveTelemetryShortcut(chord));
        setMessage('Telemetry shortcut saved. It works only in the main window.');
      });
      return;
    }
    saveRecordedGlobalShortcut(action, chord);
  };

  const saveGeneral = (trayEnabled: boolean, autoSelectAudio: boolean) =>
    void perform(async () => {
      setSnapshot(await desktopApi.saveGeneralPreferences(trayEnabled, autoSelectAudio));
      setMessage('Preferences saved.');
    });

  const isShortcutControlDisabled = busy || editing !== null;
  let body = null;
  if (snapshot && section === 'general') {
    const { preferences, settingsDirectory } = snapshot;
    body = (
      <div className="space-y-8">
        <section aria-labelledby="storage-heading" className="space-y-3">
          <h2 id="storage-heading" className="font-semibold">
            Settings storage
          </h2>
          <p className="text-sm text-muted-foreground leading-relaxed">
            Keep your preferences, stream settings and onboarding state in a folder you choose. Existing settings are
            copied safely; originals are kept as a backup.
          </p>
          <form
            className="flex flex-wrap gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              const path = pathInputRef.current?.value ?? '';
              void perform(async () => {
                setSnapshot(await desktopApi.setSettingsDirectory(path));
                setMessage('Settings folder confirmed.');
              });
            }}
          >
            <Input
              key={settingsDirectory}
              ref={pathInputRef}
              aria-label="Settings folder path"
              defaultValue={settingsDirectory}
              disabled={busy}
              spellCheck={false}
              className="min-w-0 flex-1 basis-64 font-mono text-xs"
            />
            <Button type="submit" variant="outline" disabled={busy}>
              Apply
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() =>
                void perform(async () => {
                  setSnapshot(await desktopApi.chooseSettingsDirectory());
                  setMessage('Settings folder confirmed.');
                })
              }
            >
              <FolderOpen aria-hidden="true" />
              Choose folder…
            </Button>
          </form>
          <p className="text-xs text-muted-foreground">
            Choose a folder without existing Slopcast settings. Packaged server configuration, logs and browser data are
            not moved.
          </p>
        </section>
        <section
          aria-labelledby="tray-heading"
          className="flex flex-wrap items-start justify-between gap-4 border-t border-border pt-6 sm:gap-6"
        >
          <div className="space-y-2">
            <h2 id="tray-heading" className="font-semibold">
              Tray icon
            </h2>
            <p id="tray-hint" className="text-sm text-muted-foreground leading-relaxed">
              Control sharing, mute audio or quit from the tray. When enabled, closing the main window keeps Slopcast
              running. Quit ends sharing and closes your room.
            </p>
          </div>
          <Switch
            aria-labelledby="tray-heading"
            aria-describedby="tray-hint"
            checked={preferences.trayEnabled}
            disabled={busy}
            onCheckedChange={(value) => saveGeneral(value, preferences.autoSelectAudio)}
          />
        </section>
        <section
          aria-labelledby="audio-heading"
          className="flex flex-wrap items-start justify-between gap-4 border-t border-border pt-6 sm:gap-6"
        >
          <div className="space-y-2">
            <h2 id="audio-heading" className="font-semibold">
              Select application audio automatically
            </h2>
            <p id="audio-hint" className="text-sm text-muted-foreground leading-relaxed">
              Match audio to the application you share. If no single application matches, share video only. Turn this
              off to start with No Audio; you can always choose a source manually.
            </p>
            <p className="text-xs text-muted-foreground">
              Your explicit audio choice is preserved. All system sound is never selected automatically.
            </p>
          </div>
          <Switch
            aria-labelledby="audio-heading"
            aria-describedby="audio-hint"
            checked={preferences.autoSelectAudio}
            disabled={busy}
            onCheckedChange={(value) => saveGeneral(preferences.trayEnabled, value)}
          />
        </section>
      </div>
    );
  }
  if (snapshot && section === 'stream') body = <StreamSettingsPanel />;
  if (snapshot && draft && section === 'shortcuts') {
    const telemetryShortcut = snapshot.preferences.telemetryShortcut;
    let telemetryControls: ReactNode;
    if (editing === 'toggle-telemetry') {
      telemetryControls = (
        <>
          <Button
            ref={recordButtonRef}
            variant="outline"
            aria-labelledby="telemetry-shortcut-heading"
            onKeyDown={(event) => recordShortcut(event, 'toggle-telemetry')}
          >
            Press shortcut…
          </Button>
          <Button variant="ghost" disabled={busy} onClick={() => void perform(endRecording)}>
            Cancel
          </Button>
        </>
      );
    } else {
      telemetryControls = (
        <>
          <Button
            variant="outline"
            disabled={isShortcutControlDisabled}
            aria-label="Edit shortcut for Toggle telemetry overlay"
            data-shortcut-action="toggle-telemetry"
            onClick={() =>
              void perform(async () => {
                await desktopApi.setShortcutRecording(true);
                setEditing('toggle-telemetry');
                setMessage('Press your new shortcut. Escape cancels; Tab moves to Cancel.');
              })
            }
          >
            <kbd className="font-mono text-xs">{formatShortcut(telemetryShortcut)}</kbd>
          </Button>
          <Button
            variant="ghost"
            size="sm"
            disabled={isShortcutControlDisabled || !telemetryShortcut}
            aria-label="Clear shortcut for Toggle telemetry overlay"
            onClick={() =>
              void perform(async () => {
                setSnapshot(await desktopApi.saveTelemetryShortcut(''));
                setMessage('Telemetry shortcut disabled.');
              })
            }
          >
            Clear
          </Button>
          <Button
            variant="ghost"
            size="sm"
            disabled={isShortcutControlDisabled}
            aria-label="Reset shortcut for Toggle telemetry overlay"
            onClick={() =>
              void perform(async () => {
                setSnapshot(await desktopApi.saveTelemetryShortcut(null));
                setMessage('Telemetry shortcut reset.');
              })
            }
          >
            Reset
          </Button>
        </>
      );
    }
    body = (
      <div className="space-y-5">
        <div className="space-y-2">
          <h2 className="font-semibold">Global keyboard shortcuts</h2>
          <p className="text-sm text-muted-foreground leading-relaxed">
            Use these even when Slopcast is not focused. Start opens a preview; press it again once the frame is ready
            to go live. Stop ends capture immediately, leaving your room open.
          </p>
        </div>
        <ShortcutStatus report={report} />
        <div className="divide-y divide-border border-y border-border">
          {SHORTCUT_ACTIONS.map((action) => {
            let controls: ReactNode;
            if (editing === action.id) {
              controls = (
                <>
                  <Button
                    ref={recordButtonRef}
                    variant="outline"
                    aria-labelledby={`shortcut-${action.id}`}
                    onKeyDown={(event) => recordShortcut(event, action.id)}
                  >
                    Press shortcut…
                  </Button>
                  <Button variant="ghost" disabled={busy} onClick={() => void perform(endRecording)}>
                    Cancel
                  </Button>
                </>
              );
            } else {
              controls = (
                <>
                  <Button
                    variant="outline"
                    disabled={isShortcutControlDisabled}
                    aria-label={`Edit shortcut for ${action.label}`}
                    data-shortcut-action={action.id}
                    onClick={() =>
                      void perform(async () => {
                        await desktopApi.setShortcutRecording(true);
                        setEditing(action.id);
                        setMessage('Press your new shortcut. Escape cancels; Tab moves to Cancel.');
                      })
                    }
                  >
                    <kbd className="font-mono text-xs">{formatShortcut(draft[action.id])}</kbd>
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy || editing !== null || !draft[action.id]}
                    aria-label={`Clear shortcut for ${action.label}`}
                    onClick={() => setDraft({ ...draft, [action.id]: '' })}
                  >
                    Clear
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={isShortcutControlDisabled}
                    aria-label={`Reset shortcut for ${action.label}`}
                    onClick={() => setDraft({ ...draft, [action.id]: action.defaultChord })}
                  >
                    Reset
                  </Button>
                </>
              );
            }
            return (
              <div key={action.id} className="py-4 space-y-2">
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <span id={`shortcut-${action.id}`} className="text-sm font-medium">
                    {action.label}
                  </span>
                  <div className="flex flex-wrap items-center gap-2">{controls}</div>
                </div>
                <p className="text-xs text-muted-foreground">
                  Active: <span className="font-mono">{formatShortcut(report?.bindings[action.id] ?? '')}</span>
                </p>
              </div>
            );
          })}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button
            disabled={isShortcutControlDisabled}
            onClick={() =>
              void perform(async () => {
                setReport(await desktopApi.saveShortcuts(draft));
                await refresh();
                setMessage('Shortcut request saved. Check the active bindings above.');
              })
            }
          >
            {busy ? 'Applying…' : 'Apply shortcuts'}
          </Button>
          {report?.canConfigure && (
            <Button
              variant="outline"
              disabled={isShortcutControlDisabled}
              onClick={() =>
                void perform(async () => {
                  await desktopApi.configurePortalShortcuts();
                  setMessage('Desktop shortcut editor opened. Changes appear in the active bindings.');
                })
              }
            >
              Open desktop shortcut editor…
            </Button>
          )}
        </div>
        <p className="text-xs text-muted-foreground">
          Use Ctrl or Cmd with Shift or Alt. Clear disables an action. Registration conflicts keep your previous active
          bindings.
        </p>
        <section aria-labelledby="local-shortcuts-heading" className="space-y-3 border-t border-border pt-6">
          <h2 id="local-shortcuts-heading" className="font-semibold">
            App keyboard shortcut
          </h2>
          <p className="text-sm text-muted-foreground leading-relaxed">
            Toggle the telemetry overlay while the main Slopcast window is focused. Streaming and audio are unaffected.
          </p>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <span id="telemetry-shortcut-heading" className="text-sm font-medium">
              Toggle telemetry overlay
            </span>
            <div className="flex flex-wrap items-center gap-2">{telemetryControls}</div>
          </div>
        </section>
      </div>
    );
  }

  return (
    <div className="relative h-screen flex flex-col overflow-hidden bg-background text-foreground">
      <WindowResizeHandles />
      <TitleBar title="Settings" />
      <div className="shrink-0 border-b border-border px-4 pt-5 sm:px-6">
        <p className="text-sm text-muted-foreground">Changes take effect without restarting</p>
        <nav aria-label="Settings sections" className="mt-4 flex flex-wrap gap-1">
          {SETTINGS_SECTIONS.map(({ id, label, icon: Icon }) => (
            <Button
              key={id}
              variant={section === id ? 'secondary' : 'ghost'}
              aria-current={section === id ? 'page' : undefined}
              className="rounded-b-none"
              disabled={editing !== null}
              onClick={() => setSection(id)}
            >
              <Icon aria-hidden="true" />
              {label}
            </Button>
          ))}
        </nav>
      </div>
      <main className="min-h-0 flex-1 overflow-y-auto p-4 sm:p-6">
        {!snapshot && <PreferencesLoadState error={loadError} refresh={refresh} />}
        {body}
        {snapshot && section === 'appearance' && (
          <AppearanceSettings
            preferences={snapshot.preferences}
            busy={busy}
            onChange={(mode, theme) =>
              void perform(async () => {
                setSnapshot(await desktopApi.saveAppearancePreferences(mode, theme));
                setMessage('Appearance saved.');
              })
            }
            onTypographyChange={(typography) =>
              void perform(async () => {
                setSnapshot(await desktopApi.saveTypographyPreferences(typography));
                setMessage('Typography saved.');
              })
            }
          />
        )}
      </main>
      <footer className="shrink-0 border-t border-border px-4 py-3 min-h-12 sm:px-6">
        <p className="text-sm text-destructive" role="alert">
          {error}
        </p>
        <p className="text-sm text-muted-foreground" role="status" aria-live="polite">
          {message}
        </p>
      </footer>
    </div>
  );
}
