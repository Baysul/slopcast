const native = require('./index.js');
const assert = require('assert');

console.log('🧪 Testing Native Rust Audio Module via NAPI...');

// 1. Test initEngine
const initMsg = native.initEngine();
console.log(`✅ Engine init message: "${initMsg}"`);
assert.strictEqual(initMsg, 'Native engine initialized');

// ---------------------------------------------------------------------------
// Legacy API (backwards compatibility)
// ---------------------------------------------------------------------------

// 2. Test getAudioApplications
const apps = native.getAudioApplications();
console.log(`✅ Queried active audio applications (${apps.length} found):`, apps);
assert.ok(Array.isArray(apps), 'apps should be an Array');

// 3. Test startAudioCapture with an exclusive target node ID
const started = native.startAudioCapture(100);
console.log(`✅ startAudioCapture(100): ${started}`);
assert.strictEqual(started, true);

// 4. Test isAudioCaptureActive
const active = native.isAudioCaptureActive();
console.log(`✅ isAudioCaptureActive(): ${active}`);
assert.strictEqual(active, true);

// 5. Test stopAudioCapture
const stopped = native.stopAudioCapture();
console.log(`✅ stopAudioCapture(): ${stopped}`);
assert.strictEqual(stopped, true);

const activeAfterStop = native.isAudioCaptureActive();
console.log(`✅ isAudioCaptureActive() after stop: ${activeAfterStop}`);
assert.strictEqual(activeAfterStop, false);

// ---------------------------------------------------------------------------
// Unified cross-platform API (Task 4)
// ---------------------------------------------------------------------------

// 6. Test listAudioApplications
const listedApps = native.listAudioApplications();
console.log(`✅ listAudioApplications() (${listedApps.length} found):`, listedApps);
assert.ok(Array.isArray(listedApps), 'listedApps should be an Array');

// 7. Test startAudioCapture with a string (numeric) target ID
const startedUnified = native.startAudioCapture('100');
console.log(`✅ startAudioCapture("100"): ${startedUnified}`);
assert.strictEqual(startedUnified, true);

// 7b. Invalid targets must be rejected
assert.throws(
  () => native.startAudioCapture('not-a-node-id'),
  /node ID|process ID|bundle identifier/i,
  'an unparseable target must be rejected'
);
console.log('✅ startAudioCapture("not-a-node-id") correctly rejected');

// 8. Capture should be active again
const activeUnified = native.isAudioCaptureActive();
console.log(`✅ isAudioCaptureActive() (unified): ${activeUnified}`);
assert.strictEqual(activeUnified, true);

// 9. Stop via unified stopAudioCapture
const stoppedUnified = native.stopAudioCapture();
console.log(`✅ stopAudioCapture() (unified): ${stoppedUnified}`);
assert.strictEqual(stoppedUnified, true);
assert.strictEqual(native.isAudioCaptureActive(), false);

console.log('🎉 All Native Audio NAPI bindings PASSED!');
