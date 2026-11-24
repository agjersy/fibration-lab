// Runs transforms.html headless in Chromium, one configuration at a time (one browser per
// configuration, closed before the next), and writes results-transforms.json and one
// screenshot per scene.
//
//   node mathimator/spike/render/measure-transforms.mjs [--frames 200] [--out results-transforms.json]
//
// Playwright and its Chromium must already be installed (this script never installs anything).
// On a machine with a GPU, pass --gpu to let Chromium use it instead of the software rasterizer.
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { writeFileSync } from 'node:fs';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const frames = Number(flag('frames', 200));
const outFile = flag('out', join(here, 'results-transforms.json'));
const useGpu = args.includes('--gpu');
const only = flag('only', null);

// `shot` names the screenshot; configurations without one are extra measurements of a scene.
const configs = [
  { scene: 'grid2d-linear', shot: 'shot-grid2d-linear.png' },
  { scene: 'grid2d-nonlinear', map: 'square', shot: 'shot-grid2d-nonlinear.png' },
  { scene: 'grid2d-nonlinear', map: 'swirl', shot: 'shot-grid2d-swirl.png' },
  { scene: 'grid2d-nonlinear', map: 'square', segments: 16 },
  { scene: 'grid2d-nonlinear', map: 'square', segments: 256 },
  { scene: 'grid3d-linear', shot: 'shot-grid3d-linear.png' },
  { scene: 'grid3d-linear', msaa: 0 },
  { scene: 'surface3d', shot: 'shot-surface3d.png' },
  { scene: 'surface3d', msaa: 0 },
].filter((c) => !only || c.scene === only);

const launchArgs = useGpu
  ? ['--ignore-gpu-blocklist', '--enable-gpu']
  : ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'];
const results = [];
for (const cfg of configs) {
  const browser = await chromium.launch({ args: launchArgs });
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 });
    const page = await context.newPage();
    page.on('pageerror', (e) => console.error('page error:', e.message));
    const params = { scene: cfg.scene, panel: '0' };
    for (const k of ['map', 'segments', 'msaa', 'n']) if (cfg[k] !== undefined) params[k] = String(cfg[k]);
    const url = new URL(pathToFileURL(join(here, 'transforms.html')));
    if (cfg.shot) {
      // A still frame with the transformation most of the way applied.
      url.search = new URLSearchParams(params).toString();
      await page.goto(url.href);
      await page.waitForTimeout(300);
      await page.screenshot({ path: join(here, cfg.shot), scale: 'css' });
    }
    url.search = new URLSearchParams({ ...params, auto: '1', frames: String(frames) }).toString();
    await page.goto(url.href);
    const handle = await page.waitForFunction(() => window.__spikeResults, null, { timeout: 10 * 60 * 1000 });
    const r = await handle.jsonValue();
    results.push({ ...r, screenshot: cfg.shot ?? null });
    console.log(
      `${cfg.scene}${cfg.map ? ` ${cfg.map}` : ''}${cfg.segments ? ` seg=${cfg.segments}` : ''}${cfg.msaa === 0 ? ' msaa=0' : ''}: ` +
      `median ${r.drawMs.median} ms, p95 ${r.drawMs.p95}, p99 ${r.drawMs.p99}, max ${r.drawMs.max}; over 16.7 ms: ${r.framesOver16_7}/${r.frames}; ` +
      `${r.drawCalls} calls, ${r.vertices} vertices, ${r.instances} instances, compile ${r.compileMs} ms, msaa ${r.gpu.antialiasGranted}`,
    );
    await context.close();
  } finally {
    await browser.close();
  }
}
writeFileSync(outFile, JSON.stringify({ measuredAt: new Date().toISOString(), softwareRasterizer: !useGpu, results }, null, 2) + '\n');
console.log(`wrote ${outFile}`);
