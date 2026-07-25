const { readFileSync } = require('fs');
let nativeBinding = null;
const loadErrors = [];

const BINDING_VERSION = '0.1.0';

const isMusl = () => {
  if (process.platform !== 'linux') return false;
  try {
    return readFileSync('/usr/bin/ldd', 'utf-8').includes('musl');
  } catch {}
  if (process.report?.getReport) {
    process.report.excludeNetwork = true;
    const report = process.report.getReport();
    if (report?.header?.glibcVersionRuntime) return false;
    if (report?.sharedObjects?.some((f) => f.includes('libc.musl-') || f.includes('ld-musl-'))) return true;
  }
  try {
    return require('child_process').execSync('ldd --version', { encoding: 'utf8' }).includes('musl');
  } catch {}
  return false;
};

function loadTriple(triple) {
  try {
    return require(`./index.${triple}.node`);
  } catch (e) {
    loadErrors.push(e);
  }
  try {
    const binding = require(`@slopcast/native-rust-${triple}`);
    const version = require(`@slopcast/native-rust-${triple}/package.json`).version;
    if (
      version !== BINDING_VERSION &&
      process.env.NAPI_RS_ENFORCE_VERSION_CHECK &&
      process.env.NAPI_RS_ENFORCE_VERSION_CHECK !== '0'
    ) {
      throw new Error(
        `Native binding package version mismatch, expected ${BINDING_VERSION} but got ${version}. You can reinstall dependencies to fix this issue.`,
      );
    }
    return binding;
  } catch (e) {
    loadErrors.push(e);
  }
  return null;
}

const triplesFor = (platform, arch) => {
  switch (platform) {
    case 'linux': {
      const musl = isMusl();
      switch (arch) {
        case 'x64':
          return [musl ? 'linux-x64-musl' : 'linux-x64-gnu'];
        case 'arm64':
          return [musl ? 'linux-arm64-musl' : 'linux-arm64-gnu'];
        case 'arm':
          return [musl ? 'linux-arm-musleabihf' : 'linux-arm-gnueabihf'];
        case 'loong64':
          return [musl ? 'linux-loong64-musl' : 'linux-loong64-gnu'];
        case 'riscv64':
          return [musl ? 'linux-riscv64-musl' : 'linux-riscv64-gnu'];
        case 'ppc64':
          return ['linux-ppc64-gnu'];
        case 's390x':
          return ['linux-s390x-gnu'];
        default:
          return null;
      }
    }
    case 'win32': {
      const isGNU =
        process.config?.variables?.shlib_suffix === 'dll.a' ||
        process.config?.variables?.node_target_type === 'shared_library';
      switch (arch) {
        case 'x64':
          return [isGNU ? 'win32-x64-gnu' : 'win32-x64-msvc'];
        case 'ia32':
          return ['win32-ia32-msvc'];
        case 'arm64':
          return ['win32-arm64-msvc'];
        default:
          return null;
      }
    }
    case 'darwin': {
      switch (arch) {
        case 'x64':
          return ['darwin-universal', 'darwin-x64'];
        case 'arm64':
          return ['darwin-universal', 'darwin-arm64'];
        default:
          return null;
      }
    }
    case 'freebsd': {
      switch (arch) {
        case 'x64':
          return ['freebsd-x64'];
        case 'arm64':
          return ['freebsd-arm64'];
        default:
          return null;
      }
    }
    case 'android': {
      switch (arch) {
        case 'arm64':
          return ['android-arm64'];
        case 'arm':
          return ['android-arm-eabi'];
        default:
          return null;
      }
    }
    case 'openharmony': {
      switch (arch) {
        case 'arm64':
          return ['openharmony-arm64'];
        case 'x64':
          return ['openharmony-x64'];
        case 'arm':
          return ['openharmony-arm'];
        default:
          return null;
      }
    }
    default:
      return null;
  }
};

function requireNative() {
  if (process.env.NAPI_RS_NATIVE_LIBRARY_PATH) {
    try {
      return require(process.env.NAPI_RS_NATIVE_LIBRARY_PATH);
    } catch (err) {
      loadErrors.push(err);
    }
  }

  const triples = triplesFor(process.platform, process.arch);
  if (!triples) {
    loadErrors.push(new Error(`Unsupported OS: ${process.platform}, architecture: ${process.arch}`));
    return null;
  }

  for (const triple of triples) {
    const binding = loadTriple(triple);
    if (binding) return binding;
  }

  return null;
}

nativeBinding = requireNative();

// NAPI_RS_FORCE_WASI is a tri-state flag:
//   unset / any other value → native is preferred, WASI is only a fallback
//   'true'                   → force WASI fallback even if native loaded
//   'error'                  → force WASI and throw if no WASI binding is found
const forceWasi = process.env.NAPI_RS_FORCE_WASI === 'true' || process.env.NAPI_RS_FORCE_WASI === 'error';

if (!nativeBinding || forceWasi) {
  let wasiError = null;
  try {
    nativeBinding = require('./index.wasi.cjs');
  } catch (e) {
    wasiError = e;
  }
  if (!nativeBinding || forceWasi) {
    try {
      nativeBinding = require('@slopcast/native-rust-wasm32-wasi');
    } catch (e) {
      if (wasiError) e.cause = wasiError;
      wasiError = e;
    }
  }
  if (forceWasi && !nativeBinding) {
    const error = new Error('WASI binding not found and NAPI_RS_FORCE_WASI is set to error');
    error.cause = wasiError;
    throw error;
  }
}

if (!nativeBinding) {
  if (loadErrors.length > 0) {
    const error = new Error(
      'Cannot find native binding. ' +
        'npm has a bug related to optional dependencies (https://github.com/npm/cli/issues/4828). ' +
        'Please try `npm i` again after removing both package-lock.json and node_modules directory.',
    );
    error.cause = loadErrors.reduce((err, cur) => {
      cur.cause = err;
      return cur;
    });
    throw error;
  }
  throw new Error('Failed to load native binding');
}

module.exports = nativeBinding;
