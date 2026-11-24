// Evaluates generated GLSL in headless Chromium (WebGL2) for tests/test_glsl.py.
// stdin:  {"cases": [{"id", "header", "glsl", "main", "xs": [...], "ys": [...], "uniforms": {name: value}}]}
// stdout: {"renderer": string, "results": {id: [[r, g], ...] | {"error": string}}}
// Non-finite numbers are written as the strings "nan", "inf", "-inf".
// Exit code 3 means WebGL2 with float render targets is not available (the test skips).
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch (e) {
  console.error('playwright is not available: ' + e.message);
  process.exit(3);
}

const N = 64;

function inPage({ cases, N }) {
  const canvas = document.createElement('canvas');
  const gl = canvas.getContext('webgl2');
  if (!gl || !gl.getExtension('EXT_color_buffer_float')) return { unavailable: true };
  const info = gl.getExtension('WEBGL_debug_renderer_info');
  const renderer = info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);

  const tex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, tex);
  gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA32F, N, 1);
  const fb = gl.createFramebuffer();
  gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
  gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
  gl.viewport(0, 0, N, 1);

  const VS = `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;
  function compile(type, src) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    return s;
  }
  const enc = (v) => (Number.isNaN(v) ? 'nan' : v === Infinity ? 'inf' : v === -Infinity ? '-inf' : v);
  const results = {};
  for (const c of cases) {
    try {
      const fs = `${c.header}${c.glsl}
uniform float q_xs[${N}];
uniform float q_ys[${N}];
out vec4 q_out;
void main() {
  int i = int(gl_FragCoord.x);
  float x = q_xs[i];
  float y = q_ys[i];
  q_out = ${c.main};
}`;
      const prog = gl.createProgram();
      gl.attachShader(prog, compile(gl.VERTEX_SHADER, VS));
      gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, fs));
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
      gl.useProgram(prog);
      const pad = (a) => { const f = new Float32Array(N); f.set(a || []); return f; };
      gl.uniform1fv(gl.getUniformLocation(prog, 'q_xs'), pad(c.xs));
      const ysLoc = gl.getUniformLocation(prog, 'q_ys');
      if (ysLoc) gl.uniform1fv(ysLoc, pad(c.ys));
      for (const [name, value] of Object.entries(c.uniforms || {})) {
        gl.uniform1f(gl.getUniformLocation(prog, name), value);
      }
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      const px = new Float32Array(N * 4);
      gl.readPixels(0, 0, N, 1, gl.RGBA, gl.FLOAT, px);
      const rows = [];
      for (let i = 0; i < c.xs.length; i++) rows.push([enc(px[4 * i]), enc(px[4 * i + 1])]);
      results[c.id] = rows;
    } catch (e) {
      results[c.id] = { error: String(e.message || e) };
    }
  }
  return { renderer, results };
}

(async () => {
  let input = '';
  for await (const chunk of process.stdin) input += chunk;
  const { cases } = JSON.parse(input);
  let browser;
  try {
    browser = await chromium.launch({
      args: ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'],
    });
  } catch (e) {
    console.error('cannot launch Chromium: ' + e.message);
    process.exit(3);
  }
  try {
    const page = await browser.newPage();
    const out = await page.evaluate(inPage, { cases, N });
    if (out.unavailable) {
      console.error('WebGL2 with float render targets is not available');
      process.exit(3);
    }
    process.stdout.write(JSON.stringify(out) + '\n');
  } finally {
    await browser.close();
  }
})().catch((e) => { console.error(e); process.exit(1); });
