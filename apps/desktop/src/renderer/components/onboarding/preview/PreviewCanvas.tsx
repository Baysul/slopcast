import { useEffect, useRef } from 'react';
import type { PreviewFrame } from '../../../types';

// Live preview renderer: raw BGRA frames (tightly packed, native DMA-BUF
// readback byte order) are uploaded into a persistent GPU texture and drawn
// to the card — no per-frame decode, no channel shuffling.
//
// Pipeline preference:
// 1. WebGPU — the intended path: a persistent `bgra8unorm` texture updated
//    per frame with `queue.writeTexture` (pixels land in GPU memory as-is).
//    The Linux webview (WebKitGTK 2.52.x) builds with `ENABLE_WEBGPU=OFF`
//    and exposes no runtime toggle, so `navigator.gpu` is undefined there —
//    this path activates automatically wherever a future webview enables it.
// 2. WebGL2 — the equivalent in the shipped webview: the raw BGRA bytes are
//    uploaded with `texImage2D`/`texSubImage2D` labeled as RGBA, and the
//    channel order is corrected at sample time in the fragment shader
//    (`.bgra`). WebKitGTK's WebGL2 rejects `TEXTURE_SWIZZLE` (INVALID_ENUM)
//    and does not expose `EXT_texture_format_BGRA8888`, so the shader is
//    the only zero-copy place for the swap. Same persistent-texture +
//    per-frame-upload shape as WebGPU.
// 3. Canvas2D — last resort: JS-side R/B swap into an ImageData.
//
// The native side already scales frames to fit the card (OBS-style), so the
// draw only needs an aspect-preserving fit of the texture quad.

const PREVIEW_VERT = `#version 300 es
layout(location = 0) in vec2 aPos;
out vec2 vUv;
uniform vec2 uScale;
void main() {
  gl_Position = vec4(aPos * uScale, 0.0, 1.0);
  // WebGL texture row 0 is the image's top row, so V must run opposite to
  // clip space Y (screen top = +1 samples t = 0).
  vUv = vec2(aPos.x * 0.5 + 0.5, 0.5 - aPos.y * 0.5);
}`;

const PREVIEW_FRAG = `#version 300 es
precision mediump float;
in vec2 vUv;
out vec4 outColor;
uniform sampler2D uTex;
void main() {
  // The upload is labeled RGBA but the raw bytes are BGRA — the channel
  // swap happens here at sample time (WebKitGTK's WebGL2 rejects
  // TEXTURE_SWIZZLE and lacks EXT_texture_format_BGRA8888, so the shader
  // is the only zero-copy place to do it).
  outColor = texture(uTex, vUv).bgra;
}`;

interface GpuFit {
  scaleX: number;
  scaleY: number;
}

/** Aspect-preserving fit of a `texW x texH` quad into a `canvasW x canvasH`
 * viewport (letterbox); returns the clip-space scale to apply to a
 * full-canvas quad. */
function fitScale(texW: number, texH: number, canvasW: number, canvasH: number): GpuFit {
  if (texW === 0 || texH === 0 || canvasW === 0 || canvasH === 0) {
    return { scaleX: 1, scaleY: 1 };
  }
  const texAspect = texW / texH;
  const canvasAspect = canvasW / canvasH;
  if (texAspect > canvasAspect) {
    return { scaleX: 1, scaleY: canvasAspect / texAspect };
  }
  return { scaleX: texAspect / canvasAspect, scaleY: 1 };
}

/** Compiles a WebGL2 program; returns null on failure (logged once). */
function compileProgram(gl: WebGL2RenderingContext): WebGLProgram | null {
  const compile = (type: number, source: string): WebGLShader | null => {
    const shader = gl.createShader(type);
    if (!shader) return null;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      console.warn('[PreviewCanvas] shader compile failed:', gl.getShaderInfoLog(shader));
      gl.deleteShader(shader);
      return null;
    }
    return shader;
  };
  const vertex = compile(gl.VERTEX_SHADER, PREVIEW_VERT);
  const fragment = compile(gl.FRAGMENT_SHADER, PREVIEW_FRAG);
  if (!vertex || !fragment) return null;
  const program = gl.createProgram();
  if (!program) return null;
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.warn('[PreviewCanvas] program link failed:', gl.getProgramInfoLog(program));
    gl.deleteProgram(program);
    return null;
  }
  return program;
}

// WebGPU usage flags missing from TS's lib.dom (spec-defined bits).
const GPU_TEXTURE_USAGE_TEXTURE_BINDING = 0x04;
const GPU_TEXTURE_USAGE_COPY_DST = 0x02;
const GPU_BUFFER_USAGE_UNIFORM = 0x40;
const GPU_BUFFER_USAGE_COPY_DST = 0x08;

/** WebGL2 renderer: raw BGRA upload + TEXTURE_SWIZZLE channel fix. */
class WebGl2Preview {
  private readonly gl: WebGL2RenderingContext;
  private readonly program: WebGLProgram | null;
  private readonly vao: WebGLVertexArrayObject | null;
  private readonly buffer: WebGLBuffer | null;
  private readonly uScale: WebGLUniformLocation | null;
  private texture: WebGLTexture | null = null;
  private texW = 0;
  private texH = 0;

  constructor(gl: WebGL2RenderingContext) {
    this.gl = gl;
    const program = compileProgram(gl);
    if (!program) {
      this.program = null;
      this.vao = null;
      this.buffer = null;
      this.uScale = null;
      return;
    }
    this.program = program;
    this.uScale = gl.getUniformLocation(program, 'uScale');
    // Fullscreen triangle (clip space), covering the viewport before uScale.
    const vertices = new Float32Array([-1, -1, 3, -1, -1, 3]);
    this.vao = gl.createVertexArray();
    this.buffer = gl.createBuffer();
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.bufferData(gl.ARRAY_BUFFER, vertices, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.bindVertexArray(null);
  }

  /** Uploads one BGRA frame (tightly packed, `width * height * 4` bytes)
   * into the persistent texture, recreating it when the size changes. */
  upload(frame: PreviewFrame): void {
    const gl = this.gl;
    if (!this.program) return;
    const { width, height, data } = frame;
    if (width !== this.texW || height !== this.texH) {
      if (this.texture) gl.deleteTexture(this.texture);
      this.texture = gl.createTexture();
      this.texW = width;
      this.texH = height;
      gl.bindTexture(gl.TEXTURE_2D, this.texture);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      // The raw bytes are BGRA; they are uploaded labeled as RGBA and the
      // fragment shader swaps the channels at sample time (see PREVIEW_FRAG)
      // — zero-copy, no per-frame JS swizzle. `frame.data` is already a
      // Uint8Array view; wrapping it again would copy.
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
    } else {
      gl.bindTexture(gl.TEXTURE_2D, this.texture);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, data);
    }
  }

  /** Draws the texture quad fitted into the current canvas size. */
  draw(canvasW: number, canvasH: number): void {
    const gl = this.gl;
    if (!this.program || !this.texture || !this.vao || !this.buffer) return;
    const { scaleX, scaleY } = fitScale(this.texW, this.texH, canvasW, canvasH);
    gl.viewport(0, 0, canvasW, canvasH);
    // biome-ignore lint/correctness/useHookAtTopLevel: WebGL call, not a React hook — Biome's use* heuristic false-positives on gl.useProgram.
    gl.useProgram(this.program);
    if (this.uScale) gl.uniform2f(this.uScale, scaleX, scaleY);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
  }
}

/** WebGPU renderer: persistent `bgra8unorm` texture + `writeTexture` per
 * frame. Activates only when the webview exposes `navigator.gpu`. */
class WebGpuPreview {
  private readonly device: GPUDevice;
  private readonly context: GPUCanvasContext;
  private readonly format: GPUTextureFormat;
  private pipeline: GPURenderPipeline | null = null;
  private bindGroup: GPUBindGroup | null = null;
  private texture: GPUTexture | null = null;
  private uniform: GPUBuffer | null = null;
  private texW = 0;
  private texH = 0;

  constructor(device: GPUDevice, canvas: HTMLCanvasElement) {
    this.device = device;
    this.format = navigator.gpu.getPreferredCanvasFormat();
    this.context = canvas.getContext('webgpu') as GPUCanvasContext;
    this.context.configure({ device, format: this.format, alphaMode: 'opaque' });
  }

  upload(frame: PreviewFrame): void {
    const device = this.device;
    if (frame.width !== this.texW || frame.height !== this.texH) {
      this.texture?.destroy();
      this.texture = device.createTexture({
        size: [frame.width, frame.height],
        format: 'bgra8unorm',
        usage: GPU_TEXTURE_USAGE_TEXTURE_BINDING | GPU_TEXTURE_USAGE_COPY_DST,
      });
      this.texW = frame.width;
      this.texH = frame.height;
      this.rebuildPipeline();
    }
    if (!this.texture) return;
    device.queue.writeTexture({ texture: this.texture }, frame.data, { bytesPerRow: frame.width * 4 }, [
      frame.width,
      frame.height,
    ]);
  }

  draw(canvasW: number, canvasH: number): void {
    if (!this.pipeline || !this.bindGroup || !this.uniform) return;
    const device = this.device;
    const { scaleX, scaleY } = fitScale(this.texW, this.texH, canvasW, canvasH);
    device.queue.writeBuffer(this.uniform, 0, new Float32Array([scaleX, scaleY]));
    const encoder = device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: this.context.getCurrentTexture().createView(),
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
          loadOp: 'clear',
          storeOp: 'store',
        },
      ],
    });
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.draw(3);
    pass.end();
    device.queue.submit([encoder.finish()]);
  }

  private rebuildPipeline(): void {
    const device = this.device;
    const format = this.format;
    const shader = device.createShaderModule({
      code: `
        struct Uniforms { scale: vec2<f32>, pad: vec2<f32> };
        @group(0) @binding(0) var<uniform> u: Uniforms;
        @group(0) @binding(1) var tex: texture_2d<f32>;
        @group(0) @binding(2) var samp: sampler;
        struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
        @vertex fn vs(@builtin(vertex_index) i: u32) -> VsOut {
          var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
          var out: VsOut;
          out.pos = vec4<f32>(p[i] * u.scale, 0.0, 1.0);
          // Texture row 0 is the image's top row; V runs opposite to clip Y.
          out.uv = vec2<f32>(p[i].x * 0.5 + 0.5, 0.5 - p[i].y * 0.5);
          return out;
        }
        @fragment fn fs(in: VsOut) -> @location(0) vec4<f32> {
          return textureSample(tex, samp, in.uv);
        }
      `,
    });
    this.pipeline = device.createRenderPipeline({
      layout: 'auto',
      vertex: { module: shader, entryPoint: 'vs' },
      fragment: {
        module: shader,
        entryPoint: 'fs',
        targets: [{ format }],
      },
      primitive: { topology: 'triangle-list' },
    });
    if (!this.texture) return;
    this.uniform?.destroy();
    this.uniform = device.createBuffer({
      size: 16,
      usage: GPU_BUFFER_USAGE_UNIFORM | GPU_BUFFER_USAGE_COPY_DST,
    });
    const sampler = device.createSampler({
      magFilter: 'linear',
      minFilter: 'linear',
    });
    this.bindGroup = device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: this.uniform } },
        { binding: 1, resource: this.texture.createView() },
        { binding: 2, resource: sampler },
      ],
    });
  }
}

/** Canvas2D fallback: JS-side R/B swap into an ImageData draw. */
function drawCanvas2d(ctx: CanvasRenderingContext2D, frame: PreviewFrame): void {
  const { width, height, data } = frame;
  const image = ctx.createImageData(width, height);
  const pixels = data;
  const out = image.data;
  for (let i = 0; i < pixels.length; i += 4) {
    const [blue = 0, green = 0, red = 0, alpha = 0] = pixels.subarray(i, i + 4);
    out[i] = red; // R
    out[i + 1] = green; // G
    out[i + 2] = blue; // B
    out[i + 3] = alpha; // A
  }
  ctx.putImageData(image, 0, 0);
}

type PreviewRenderer = {
  upload: (frame: PreviewFrame) => void;
  draw: (width: number, height: number) => void;
};

/** Attempts the WebGPU renderer (unavailable in WebKitGTK builds as of
 * 2.52.x — the path stays for webviews that ship `navigator.gpu`). */
async function tryWebGpu(canvas: HTMLCanvasElement): Promise<PreviewRenderer | null> {
  if (!navigator.gpu) return null;
  try {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) return null;
    const device = await adapter.requestDevice();
    return new WebGpuPreview(device, canvas);
  } catch (err) {
    console.warn('[PreviewCanvas] WebGPU init failed, falling back to WebGL2:', err);
    return null;
  }
}

/** The shipped path in the WebKitGTK webview: raw BGRA upload + shader
 * channel swap. `preserveDrawingBuffer: true` keeps the last drawn frame
 * readable after compositing (the preview spec samples canvas pixels; a
 * default WebGL buffer is cleared post-composite). */
function tryWebGl2(canvas: HTMLCanvasElement): PreviewRenderer | null {
  const gl = canvas.getContext('webgl2', { preserveDrawingBuffer: true });
  return gl ? new WebGl2Preview(gl) : null;
}

/** Last resort: JS-side R/B swap into an ImageData draw. */
function tryCanvas2d(canvas: HTMLCanvasElement): PreviewRenderer | null {
  const ctx = canvas.getContext('2d');
  if (!ctx) return null;
  let latest: PreviewFrame | null = null;
  return {
    upload: (frame: PreviewFrame) => {
      latest = frame;
    },
    draw: () => {
      if (latest) drawCanvas2d(ctx, latest);
    },
  };
}

export const PreviewCanvas: React.FC<{ frame: PreviewFrame }> = ({ frame }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<PreviewRenderer | null>(null);
  const canvasSizeRef = useRef({ width: 0, height: 0 });

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const setup = async (): Promise<void> => {
      const gpu = await tryWebGpu(canvas);
      if (gpu) {
        rendererRef.current = gpu;
        window.__PREVIEW_RENDERER__ = 'webgpu';
        console.info('[PreviewCanvas] preview renderer: WebGPU');
        return;
      }
      // TEMP wedge-hunt: WebGL2 wedged the webview's JS main thread after a
      // few frames under the channel-era transport on this stack. Keep the
      // CPU Canvas2D path until WebGL2 is re-tested against the frame://
      // pull transport (see the freeze investigation notes).
      const canvas2d = tryCanvas2d(canvas);
      if (canvas2d) {
        rendererRef.current = canvas2d;
        window.__PREVIEW_RENDERER__ = 'canvas2d';
        console.warn('[PreviewCanvas] TEMP: forced Canvas2D renderer');
        return;
      }
      const gl = tryWebGl2(canvas);
      if (gl) {
        rendererRef.current = gl;
        window.__PREVIEW_RENDERER__ = 'webgl2';
        console.info('[PreviewCanvas] preview renderer: WebGL2');
        return;
      }
      window.__PREVIEW_RENDERER__ = 'none';
    };
    void setup();

    const resize = (): void => {
      const rect = canvas.getBoundingClientRect();
      const width = Math.max(1, Math.round(rect.width * window.devicePixelRatio));
      const height = Math.max(1, Math.round(rect.height * window.devicePixelRatio));
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      canvasSizeRef.current = { width, height };
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);

    return () => {
      observer.disconnect();
      rendererRef.current = null;
    };
  }, []);

  useEffect(() => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    renderer.upload(frame);
    const { width, height } = canvasSizeRef.current;
    if (width > 0 && height > 0) renderer.draw(width, height);
    if (window.__PREVIEW_BENCH__) {
      window.__PREVIEW_BENCH_DATA__?.push([frame.ptsUs, performance.now(), performance.now()]);
    }
  }, [frame]);

  return (
    <canvas
      ref={canvasRef}
      className="absolute inset-0 w-full h-full object-contain"
      aria-label="Live screenshare preview"
    />
  );
};
