// WebdriverIO config for the Tauri presenter e2e phase (MIGRATION §12).
// Run by the harness (`apps/server/src/e2e-test.ts`) via
// `pnpm --filter desktop exec wdio run ./wdio.conf.ts`; the `e2e` cargo
// feature must be enabled in the binary under test
// (`pnpm --filter desktop tauri build --features e2e`).
//
// The embedded driver provider needs no external driver: the app binary
// registers `tauri-plugin-wdio-webdriver` (WebDriver HTTP server on
// `TAURI_WEBDRIVER_PORT`) and `tauri-plugin-wdio` (`browser.tauri.execute`).
export const config = {
  runner: 'local',
  specs: ['./tests/e2e/presenter.spec.ts'],
  maxInstances: 1,
  capabilities: [{ browserName: 'tauri' }],
  logLevel: 'info',
  waitforTimeout: 60_000,
  framework: 'mocha',
  mochaOpts: {
    ui: 'bdd',
    // The pre-roll portal picker wait (60s) and the spectator-phase hold
    // (120s) both live inside single tests.
    timeout: 180_000,
  },
  reporters: ['spec'],
  services: [
    [
      '@wdio/tauri-service',
      {
        // Cargo workspace target dir lives at the repo root, not in src-tauri.
        // `pnpm --filter desktop exec wdio run` runs with cwd = apps/desktop,
        // so the root target dir is two levels up.
        appBinaryPath: '../../target/release/slopcast',
        driverProvider: 'embedded',
        captureBackendLogs: true,
        backendLogLevel: 'info',
      },
    ],
  ],
};
