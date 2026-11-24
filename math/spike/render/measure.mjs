// Runs index.html headless in Chromium for each configuration and writes results.json and one
// screenshot per configuration.
//
//   node math/spike/render/measure.mjs [--frames 600] [--out results.json]
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
const frames = Number(flag('frames', 600));
const outFile = flag('out', join(here, 'results.json'));
const useGpu = args.includes('--gpu');

const screens = [
  { name: '1280x800', width: 1280, height: 800, dpr: 1 },
  { name: '2560x1600', width: 1280, height: 800, dpr: 2 },
];
const scenes = [
  { curves: 20, implicit: 0 },
  { curves: 20, implicit: 1 },
  { curves: 100, implicit: 0 },
  { curves: 100, implicit: 1 },
];
// A sweep of the samples per curve at the smaller screen, to show what the cost scales with.
const sweep = [512, 1024, 4096].map((samples) => ({ curves: 20, implicit: 0, samples }));

const launchArgs = useGpu
  ? ['--ignore-gpu-blocklist', '--enable-gpu']
  : ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'];
const browser = await chromium.launch({ args: launchArgs });
const results = [];
try {
  for (const screen of screens) {
    for (const scene of screen.dpr === 1 ? [...scenes, ...sweep] : scenes) {
      const context = await browser.newContext({
        viewport: { width: screen.width, height: screen.height },
        deviceScaleFactor: screen.dpr,
      });
      const page = await context.newPage();
      page.on('pageerror', (e) => console.error('page error:', e.message));
      const url = new URL(pathToFileURL(join(here, 'index.html')));
      url.search = new URLSearchParams({
        auto: '1', panel: '0', curves: String(scene.curves), implicit: String(scene.implicit), frames: String(frames),
        samples: String(scene.samples ?? 2048),
      }).toString();
      await page.goto(url.href);
      const handle = await page.waitForFunction(() => window.__spikeResults, null, { timeout: 15 * 60 * 1000 });
      const result = await handle.jsonValue();
      const shot = scene.samples ? null : `shot-${screen.name}-${scene.curves}curves-${scene.implicit ? 'implicit' : 'plain'}.png`;
      if (shot) await page.screenshot({ path: join(here, shot), scale: 'css' });
      results.push({ screen: screen.name, ...result, screenshot: shot });
      console.log(
        `${screen.name} ${String(scene.curves).padStart(3)} curves × ${result.config.samples} implicit=${scene.implicit}: ` +
        `draw median ${result.drawMs.median} ms, p95 ${result.drawMs.p95}, p99 ${result.drawMs.p99}, max ${result.drawMs.max}; ` +
        `over 16.7 ms: ${result.framesOver16_7}/${result.frames}; dropped ${result.droppedFrames}`,
      );
      await context.close();
    }
  }
} finally {
  await browser.close();
}
writeFileSync(outFile, JSON.stringify({ measuredAt: new Date().toISOString(), softwareRasterizer: !useGpu, results }, null, 2) + '\n');
console.log(`wrote ${outFile}`);
