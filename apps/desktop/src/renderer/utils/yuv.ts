// I420 plane handling for the raw preview path: the backend ships tightly
// packed Y/U/V planes (stride == width) as one base64 payload; this module
// decodes them and exposes them as WebGL textures.

export interface I420Frame {
  /** Tightly packed planes, concatenated: Y (w*h), U (w*h/4), V (w*h/4). */
  data: Uint8Array;
  width: number;
  height: number;
}

const decodeBase64 = (data: string): Uint8Array => {
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
};

/** Validates and splits a base64 I420 payload into its planes. Throws when
 * the payload is too small for the declared dimensions. Chroma dims round up
 * (div_ceil), matching the native plane packing for odd-sized frames. */
export function parseI420Frame(data: string, width: number, height: number): I420Frame {
  const bytes = decodeBase64(data);
  const yLen = width * height;
  const uvLen = Math.ceil(width / 2) * Math.ceil(height / 2);
  const expected = yLen + 2 * uvLen;
  if (bytes.length < expected) {
    throw new Error(`I420 payload too small: ${bytes.length} < ${expected}`);
  }
  return { data: bytes, width, height };
}

// BT.601 limited-range constants — must match libyuv's ARGBToI420, which
// produced the planes on the native side.
const YUV_TO_RGB_VERTEX = `
  attribute vec2 a_pos;
  varying vec2 v_uv;
  void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
  }
`;

const YUV_TO_RGB_FRAGMENT = `
  precision mediump float;
  uniform sampler2D u_y;
  uniform sampler2D u_u;
  uniform sampler2D u_v;
  uniform vec2 u_uv_scale;
  varying vec2 v_uv;
  void main() {
    float y = texture2D(u_y, v_uv).r;
    float u = texture2D(u_u, v_uv * u_uv_scale).r - 0.5;
    float v = texture2D(u_v, v_uv * u_uv_scale).r - 0.5;
    float r = y + 1.596027 * v;
    float g = y - 0.391762 * u - 0.812968 * v;
    float b = y + 2.017232 * u;
    gl_FragColor = vec4(r, g, b, 1.0);
  }
`;

interface WebGlContext {
  gl: WebGL2RenderingContext;
  program: WebGLProgram;
  yTex: WebGLTexture;
  uTex: WebGLTexture;
  vTex: WebGLTexture;
  aPos: number;
  uY: WebGLUniformLocation;
  uU: WebGLUniformLocation;
  uV: WebGLUniformLocation;
  uvScale: WebGLUniformLocation;
  buffer: WebGLBuffer;
}

let cachedContext: WebGlContext | null = null;

const compileShader = (gl: WebGL2RenderingContext, type: number, source: string): WebGLShader => {
  const shader = gl.createShader(type);
  if (!shader) throw new Error('WebGL shader creation failed');
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const info = gl.getShaderInfoLog(shader);
    gl.deleteShader(shader);
    throw new Error(`WebGL shader compile failed: ${info}`);
  }
  return shader;
};

/** Lazily builds the YUV→RGB WebGL2 program on the given canvas. Throws when
 * WebGL2 is unavailable (the caller falls back to a status message). */
function acquireContext(canvas: HTMLCanvasElement): WebGlContext {
  if (cachedContext) return cachedContext;
  // preserveDrawingBuffer: without it the compositor clears the buffer after
  // each presented frame, so reading the canvas back (drawImage/readPixels,
  // used by the e2e preview-content check) would always see black.
  const gl = canvas.getContext('webgl2', { preserveDrawingBuffer: true });
  if (!gl) throw new Error('WebGL2 is unavailable in this webview');

  const program = gl.createProgram();
  if (!program) throw new Error('WebGL program creation failed');
  gl.attachShader(program, compileShader(gl, gl.VERTEX_SHADER, YUV_TO_RGB_VERTEX));
  gl.attachShader(program, compileShader(gl, gl.FRAGMENT_SHADER, YUV_TO_RGB_FRAGMENT));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const info = gl.getProgramInfoLog(program);
    throw new Error(`WebGL program link failed: ${info}`);
  }

  const makeTexture = (): WebGLTexture => {
    const tex = gl.createTexture();
    if (!tex) throw new Error('WebGL texture creation failed');
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return tex;
  };

  const buffer = gl.createBuffer();
  if (!buffer) throw new Error('WebGL buffer creation failed');
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);

  const ctx: WebGlContext = {
    gl,
    program,
    yTex: makeTexture(),
    uTex: makeTexture(),
    vTex: makeTexture(),
    aPos: gl.getAttribLocation(program, 'a_pos'),
    uY: gl.getUniformLocation(program, 'u_y') as WebGLUniformLocation,
    uU: gl.getUniformLocation(program, 'u_u') as WebGLUniformLocation,
    uV: gl.getUniformLocation(program, 'u_v') as WebGLUniformLocation,
    uvScale: gl.getUniformLocation(program, 'u_uv_scale') as WebGLUniformLocation,
    buffer,
  };
  cachedContext = ctx;
  return ctx;
}

const uploadPlane = (
  gl: WebGL2RenderingContext,
  tex: WebGLTexture,
  width: number,
  height: number,
  data: Uint8Array,
): void => {
  gl.bindTexture(gl.TEXTURE_2D, tex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, width, height, 0, gl.RED, gl.UNSIGNED_BYTE, data);
};

/** Draws one I420 frame onto the canvas (GPU-scaled to the canvas CSS size).
 * Reuses the program and textures across frames. Throws on malformed input. */
export function drawI420Frame(canvas: HTMLCanvasElement, frame: I420Frame): void {
  if (frame.width % 2 !== 0 || frame.height % 2 !== 0) {
    throw new Error(`I420 frame must be even-sized, got ${frame.width}x${frame.height}`);
  }
  const ctx = acquireContext(canvas);
  const { gl } = ctx;
  const { width, height } = frame;
  const uvW = Math.ceil(width / 2);
  const uvH = Math.ceil(height / 2);
  const yLen = width * height;
  const uvLen = uvW * uvH;

  uploadPlane(gl, ctx.yTex, width, height, frame.data.subarray(0, yLen));
  uploadPlane(gl, ctx.uTex, uvW, uvH, frame.data.subarray(yLen, yLen + uvLen));
  uploadPlane(gl, ctx.vTex, uvW, uvH, frame.data.subarray(yLen + uvLen, yLen + 2 * uvLen));

  gl.viewport(0, 0, canvas.width, canvas.height);
  // Aliased because Biome's useHookAtTopLevel rule treats any `useX(...)` call
  // as a React hook, and the WebGL2 method happens to be named `useProgram`.
  const activateProgram = gl.useProgram.bind(gl);
  activateProgram(ctx.program);
  gl.bindBuffer(gl.ARRAY_BUFFER, ctx.buffer);
  gl.enableVertexAttribArray(ctx.aPos);
  gl.vertexAttribPointer(ctx.aPos, 2, gl.FLOAT, false, 0, 0);
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, ctx.yTex);
  gl.uniform1i(ctx.uY, 0);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, ctx.uTex);
  gl.uniform1i(ctx.uU, 1);
  gl.activeTexture(gl.TEXTURE2);
  gl.bindTexture(gl.TEXTURE_2D, ctx.vTex);
  gl.uniform1i(ctx.uV, 2);
  gl.uniform2f(ctx.uvScale, width / uvW, height / uvH);
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
}
