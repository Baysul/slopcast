import { isJsonObject } from '@slopcast/shared-types';
import { useEffect, useRef } from 'react';

import type { PreviewFrame } from '../../../types';

const PREVIEW_VERT = `#version 300 es
layout(location = 0) in vec2 aPos;
out vec2 vUv;
uniform vec2 uScale;
void main() {
  gl_Position = vec4(aPos * uScale, 0.0, 1.0);
  vUv = vec2(aPos.x * 0.5 + 0.5, 0.5 - aPos.y * 0.5);
}`;

const PREVIEW_FRAG = `#version 300 es
precision mediump float;
in vec2 vUv;
out vec4 outColor;
uniform sampler2D uTex;
void main() {
  outColor = texture(uTex, vUv).bgra;
}`;

interface GpuFit {
  scaleX: number;
  scaleY: number;
}

const fitScale = (texW: number, texH: number, canvasW: number, canvasH: number): GpuFit => {
  if (texW === 0 || texH === 0 || canvasW === 0 || canvasH === 0) {
    return { scaleX: 1, scaleY: 1 };
  }
  const texAspect = texW / texH;
  const canvasAspect = canvasW / canvasH;
  if (texAspect > canvasAspect) {
    return { scaleX: 1, scaleY: canvasAspect / texAspect };
  }
  return { scaleX: texAspect / canvasAspect, scaleY: 1 };
};

const compileProgram = (gl: WebGL2RenderingContext): WebGLProgram | null => {
  const compile = (type: number, source: string): WebGLShader | null => {
    const shader = gl.createShader(type);
    if (!shader) {
      return null;
    }
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (gl.getShaderParameter(shader, gl.COMPILE_STATUS) !== true) {
      console.warn('[PreviewCanvas] shader compile failed:', gl.getShaderInfoLog(shader));
      gl.deleteShader(shader);
      return null;
    }
    return shader;
  };
  const vertex = compile(gl.VERTEX_SHADER, PREVIEW_VERT);
  const fragment = compile(gl.FRAGMENT_SHADER, PREVIEW_FRAG);
  if (!vertex || !fragment) {
    return null;
  }
  // DOM typings mark this non-null, but WebGL may return null if creation fails.
  const createProgram = (): WebGLProgram | null => gl.createProgram();
  const program = createProgram();
  if (program === null) {
    return null;
  }
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (gl.getProgramParameter(program, gl.LINK_STATUS) !== true) {
    console.warn('[PreviewCanvas] program link failed:', gl.getProgramInfoLog(program));
    gl.deleteProgram(program);
    return null;
  }
  return program;
};

const GPU_TEXTURE_USAGE_TEXTURE_BINDING = 0x04;
const GPU_TEXTURE_USAGE_COPY_DST = 0x02;
const GPU_BUFFER_USAGE_UNIFORM = 0x40;
const GPU_BUFFER_USAGE_COPY_DST = 0x08;

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

  upload(frame: PreviewFrame): void {
    const { gl } = this;
    if (!this.program) {
      return;
    }
    const { width, height, data } = frame;
    if (width !== this.texW || height !== this.texH) {
      if (this.texture) {
        gl.deleteTexture(this.texture);
      }
      this.texture = gl.createTexture();
      this.texW = width;
      this.texH = height;
      gl.bindTexture(gl.TEXTURE_2D, this.texture);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
    } else {
      gl.bindTexture(gl.TEXTURE_2D, this.texture);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, data);
    }
  }

  draw(canvasW: number, canvasH: number): void {
    const { gl } = this;
    if (!this.program || !this.texture || !this.vao || !this.buffer) {
      return;
    }
    const { scaleX, scaleY } = fitScale(this.texW, this.texH, canvasW, canvasH);
    gl.viewport(0, 0, canvasW, canvasH);
    // Biome-ignore lint/correctness/useHookAtTopLevel: WebGL call, not a React hook — Biome's use* heuristic false-positives on gl.useProgram.
    gl.useProgram(this.program);
    if (this.uScale) {
      gl.uniform2f(this.uScale, scaleX, scaleY);
    }
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
  }
}

const isGpuCanvasContext = (value: unknown): value is GPUCanvasContext =>
  isJsonObject(value) && 'configure' in value && 'getCurrentTexture' in value;

const getGpuCanvasContext = (canvas: HTMLCanvasElement): GPUCanvasContext | null => {
  const context: unknown = canvas.getContext('webgpu');
  return isGpuCanvasContext(context) ? context : null;
};

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
    const context = getGpuCanvasContext(canvas);
    if (!context) {
      throw new Error('WebGPU canvas context unavailable');
    }
    this.context = context;
    this.context.configure({ alphaMode: 'opaque', device, format: this.format });
  }

  upload(frame: PreviewFrame): void {
    const { device } = this;
    if (frame.width !== this.texW || frame.height !== this.texH) {
      this.texture?.destroy();
      this.texture = device.createTexture({
        format: 'bgra8unorm',
        size: [frame.width, frame.height],
        usage: GPU_TEXTURE_USAGE_TEXTURE_BINDING | GPU_TEXTURE_USAGE_COPY_DST,
      });
      this.texW = frame.width;
      this.texH = frame.height;
      this.rebuildPipeline();
    }
    if (!this.texture) {
      return;
    }
    device.queue.writeTexture({ texture: this.texture }, frame.data, { bytesPerRow: frame.width * 4 }, [
      frame.width,
      frame.height,
    ]);
  }

  draw(canvasW: number, canvasH: number): void {
    if (!this.pipeline || !this.bindGroup || !this.uniform) {
      return;
    }
    const { device } = this;
    const { scaleX, scaleY } = fitScale(this.texW, this.texH, canvasW, canvasH);
    device.queue.writeBuffer(this.uniform, 0, new Float32Array([scaleX, scaleY]));
    const encoder = device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          clearValue: { a: 1, b: 0, g: 0, r: 0 },
          loadOp: 'clear',
          storeOp: 'store',
          view: this.context.getCurrentTexture().createView(),
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
    const { device } = this;
    const { format } = this;
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
          out.uv = vec2<f32>(p[i].x * 0.5 + 0.5, 0.5 - p[i].y * 0.5);
          return out;
        }
        @fragment fn fs(in: VsOut) -> @location(0) vec4<f32> {
          return textureSample(tex, samp, in.uv);
        }
      `,
    });
    this.pipeline = device.createRenderPipeline({
      fragment: {
        entryPoint: 'fs',
        module: shader,
        targets: [{ format }],
      },
      layout: 'auto',
      primitive: { topology: 'triangle-list' },
      vertex: { entryPoint: 'vs', module: shader },
    });
    if (!this.texture) {
      return;
    }
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
      entries: [
        { binding: 0, resource: { buffer: this.uniform } },
        { binding: 1, resource: this.texture.createView() },
        { binding: 2, resource: sampler },
      ],
      layout: this.pipeline.getBindGroupLayout(0),
    });
  }
}

const drawCanvas2d = (ctx: CanvasRenderingContext2D, frame: PreviewFrame): void => {
  const { width, height, data } = frame;
  const image = ctx.createImageData(width, height);
  const pixels = data;
  const out = image.data;
  for (let i = 0; i < pixels.length; i += 4) {
    const [blue = 0, green = 0, red = 0, alpha = 0] = pixels.subarray(i, i + 4);
    out[i] = red;
    out[i + 1] = green;
    out[i + 2] = blue;
    out[i + 3] = alpha;
  }
  ctx.putImageData(image, 0, 0);
};

interface PreviewRenderer {
  upload: (frame: PreviewFrame) => void;
  draw: (width: number, height: number) => void;
}

const tryWebGpu = async (canvas: HTMLCanvasElement): Promise<PreviewRenderer | null> => {
  if (Object.is(navigator.gpu, null) || Object.is(navigator.gpu, undefined)) {
    return null;
  }
  try {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) {
      return null;
    }
    const device = await adapter.requestDevice();
    return new WebGpuPreview(device, canvas);
  } catch (err) {
    console.warn('[PreviewCanvas] WebGPU init failed, falling back to WebGL2:', err);
    return null;
  }
};

const tryWebGl2 = (canvas: HTMLCanvasElement): PreviewRenderer | null => {
  const gl = canvas.getContext('webgl2', { preserveDrawingBuffer: true });
  return gl ? new WebGl2Preview(gl) : null;
};

const tryCanvas2d = (canvas: HTMLCanvasElement): PreviewRenderer | null => {
  const ctx = canvas.getContext('2d');
  if (!ctx) {
    return null;
  }
  let latest: PreviewFrame | null = null;
  return {
    draw: () => {
      if (latest) {
        drawCanvas2d(ctx, latest);
      }
    },
    upload: (frame: PreviewFrame) => {
      latest = frame;
    },
  };
};

export const PreviewCanvas: React.FC<{ frame: PreviewFrame }> = ({ frame }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<PreviewRenderer | null>(null);
  const canvasSizeRef = useRef({ height: 0, width: 0 });

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return undefined;
    }

    const setup = async (): Promise<void> => {
      const gpu = await tryWebGpu(canvas);
      if (gpu) {
        rendererRef.current = gpu;
        window.previewRenderer = 'webgpu';
        console.info('[PreviewCanvas] preview renderer: WebGPU');
        return undefined;
      }
      const gl = tryWebGl2(canvas);
      if (gl) {
        rendererRef.current = gl;
        window.previewRenderer = 'webgl2';
        console.info('[PreviewCanvas] preview renderer: WebGL2');
        return undefined;
      }
      const canvas2d = tryCanvas2d(canvas);
      if (canvas2d) {
        rendererRef.current = canvas2d;
        window.previewRenderer = 'canvas2d';
        console.info('[PreviewCanvas] preview renderer: Canvas2D');
        return undefined;
      }
      window.previewRenderer = 'none';
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
      canvasSizeRef.current = { height, width };
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
    if (!renderer) {
      return;
    }
    renderer.upload(frame);
    const { width, height } = canvasSizeRef.current;
    if (width > 0 && height > 0) {
      renderer.draw(width, height);
    }
    if (window.previewBench === true) {
      window.previewBenchData?.push([frame.ptsUs, performance.now(), performance.now()]);
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
