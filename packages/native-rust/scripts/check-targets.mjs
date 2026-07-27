// Type-checks the platform modules that the host's plain `cargo check` cannot
// see behind #[cfg(target_os = "...")], so shared-struct drift (e.g. E0063)
// is caught locally instead of in CI. Coverage per host:
//   linux   -> host + windows-msvc + apple-darwin
//   darwin  -> host (macos module) + windows-msvc
//   windows -> host (windows module) only
// The linux target is never cross-checked: pipewire-sys/x11 bind against
// system headers that only exist on Linux. CI covers each host's own module.

import { spawnSync } from 'node:child_process';

const APPLE_TARGET = 'aarch64-apple-darwin';
const CROSS_TARGETS = {
  linux: ['x86_64-pc-windows-msvc', APPLE_TARGET],
  darwin: ['x86_64-pc-windows-msvc'],
  win32: [],
};

const crossTargets = CROSS_TARGETS[process.platform] ?? [];

if (crossTargets.length > 0) {
  run('rustup', ['target', 'add', ...crossTargets]);
}
run('cargo', ['check']);
for (const target of crossTargets) {
  const env = { ...process.env };
  if (target === APPLE_TARGET) {
    // screencapturekit's build script shells out to `swift`, which only exists
    // on macOS. DOCS_RS=1 skips that native bridge build; cargo check only
    // needs type information.
    env.DOCS_RS = '1';
  }
  run('cargo', ['check', '--target', target], { env });
}

function run(cmd, args, options = {}) {
  const result = spawnSync(cmd, args, { stdio: 'inherit', ...options });
  if (result.error) {
    console.error(`failed to run ${cmd}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}
