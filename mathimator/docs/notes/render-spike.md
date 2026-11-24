# Rendering spike (BUILD-SPEC WP-0.12)

*2026-11-24. Question (MQ-2): can the canvas be WebGL2 inside the system webview, or does it need a native GPU surface? Short answer: **not decided by this spike.** The prototype and the shader pipeline work, but the only renderer available here was a CPU rasterizer, which says nothing reliable about WKWebView or WebKitGTK on a real GPU. The page is ready to run in those webviews; section 5 says how.*

## 1. What was built

- **`code.glsl` in the engine** (`mathimator_engine/glsl.py`). One scene object becomes GLSL ES 3.00 with the scene's parameters as uniforms, so a slider drag is a uniform update: no recompile and no round trip to the engine. The translation is an allowlist walk that refuses what it cannot represent. Its tests run the generated GLSL in headless Chromium and compare it numerically with the engine.
- **A prototype page** (`spike/render/index.html`, self-contained, no network). It draws curves whose code comes from the engine (`gen_scene.py` writes `scene.js` through `plot.explicit` and `code.glsl`; nothing is written by hand):
  - each curve is evaluated in the **vertex shader**: x comes from `gl_VertexID`, y is `f(x)` with the uniforms, and the line is a triangle strip about two pixels wide;
  - one implicit curve and one region are evaluated in a **fragment shader** over the whole canvas;
  - a scripted "drag" changes every uniform on every frame.
- **A measurement** (`measure.mjs`). Each frame is timed around `gl.finish()`, so the time includes the GPU's work, not only issuing the commands.

The two screenshots kept in `spike/render/` were looked at: the curves, the implicit curve and the shaded region are drawn. `results.json` names six more that were not kept in the repository.

## 2. What was measured, and on what

Renderer string reported by `WEBGL_debug_renderer_info`:

> ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero)), SwiftShader driver)

That is Chromium's **software rasterizer**: every vertex and every pixel is computed on the CPU. Headless Chromium 157, 300 frames per configuration, time per frame in milliseconds. The budget for 60 frames per second is 16.7 ms.

| Canvas | Curves | Samples per curve | Fragment layer | Median | p95 | p99 | Max | Frames over 16.7 ms |
|---|---|---|---|---|---|---|---|---|
| 1280×800 | 20 | 512 | no | 10.8 | 15.3 | 22.6 | 27.4 | 11 of 300 |
| 1280×800 | 20 | 1024 | no | 15.2 | 20.0 | 22.2 | 24.3 | 75 of 300 |
| 1280×800 | 20 | 2048 | no | 24.1 | 28.9 | 44.0 | 48.4 | all |
| 1280×800 | 20 | 4096 | no | 37.3 | 66.4 | 71.1 | 73.6 | all |
| 1280×800 | 20 | 2048 | yes | 31.1 | 37.2 | 50.3 | 69.8 | all |
| 1280×800 | 100 | 2048 | no | 101.3 | 115.7 | 123.9 | 128.0 | all |
| 1280×800 | 100 | 2048 | yes | 109.5 | 125.6 | 146.2 | 158.8 | all |
| 2560×1600 | 20 | 2048 | no | 34.0 | 56.8 | 70.8 | 72.4 | all |
| 2560×1600 | 20 | 2048 | yes | 62.9 | 113.7 | 151.6 | 174.6 | all |
| 2560×1600 | 100 | 2048 | no | 141.2 | 194.0 | 261.6 | 289.5 | all |
| 2560×1600 | 100 | 2048 | yes | 175.6 | 251.3 | 278.5 | 287.5 | all |

Compiling the 21 shader programs for 20 curves took 51 ms the first time and 11–15 ms afterwards.

## 3. What this does and does not establish

**It does establish:**
- The pipeline works end to end: engine expression → GLSL → GPU evaluation with uniforms, with numerical agreement against the engine.
- The prototype needs only core WebGL2, which the system webviews are meant to provide: GLSL ES 3.00, `gl_VertexID`, `highp` floats in both shader stages, and triangle strips. It needs no extension (`WEBGL_debug_renderer_info` is read only to report the renderer). This list is what to check on WebKitGTK first.
- How cost scales when the CPU does the work: about linearly with vertices (curves × samples), plus a fragment-layer cost that grows with pixel count (7 ms at 1280×800, 29 ms at 2560×1600). On a software path the 20-curve scene meets 60 fps only at about 512 samples per curve.

**It does not establish:**
- Frame times in WKWebView or WebKitGTK. Those use the machine's GPU through a different WebGL implementation; the numbers above are neither a floor nor a ceiling for them.
- Whether WebKitGTK on a typical Linux desktop gets hardware acceleration at all. If it falls back to software rendering, the table above is roughly what a user would see, and that would decide MQ-2 in favour of a native surface.

## 4. Design consequences that hold either way

- **Sample per pixel column, not a fixed count.** 2048 samples on a 1280-pixel canvas is wasted work; one or two per column halves the vertex cost.
- **One program per object, compiled when the object is committed**, never during a drag. Compile time is small but not free.
- **Keep fragment-shader layers (implicit curves, regions) off the per-frame path** where possible: draw them only when their own parameters change, or at reduced resolution while dragging.
- **The preview is uncertified by design.** It draws a staircase's jumps as steep lines (visible in the screenshots). The certified sample from the engine must replace it when the drag ends, as ARCHITECTURE 9.1 says, and the UI should not show a "certified" badge on a preview frame.

## 5. How to finish the measurement

The same page runs anywhere. It shows the renderer string and the results, and offers them as a download.

- **macOS (WKWebView's engine):** open `mathimator/spike/render/index.html?auto=1&curves=20&samples=2048&frames=600` in Safari. Repeat with `curves=100` and with `implicit=1`.
- **Linux (WebKitGTK):** open the same URL in GNOME Web (Epiphany), which uses WebKitGTK. Note the renderer string: if it names `llvmpipe` or another software renderer, acceleration is off.
- **Inside a real Tauri window** (the most faithful test): point a minimal Tauri app's window at the same file on each platform.
- **Chromium with a GPU, as a reference:** `node mathimator/spike/render/measure.mjs --gpu`.

To regenerate the scene from the engine: `cd mathimator/python/engine && uv run python ../../spike/render/gen_scene.py`.

## 6. Recommendation

Stay with the plan of record, **WebGL2 in the webview**, as a working assumption, with **low confidence** until the two webview measurements exist. It keeps docking, undocked windows and the browser test bridge simple, and the renderer needs nothing beyond core WebGL2.

Switch to a native `wgpu` surface if either of these is observed on reference hardware:
- WebKitGTK reports a software renderer, or cannot hold 16.7 ms at p95 for 20 curves on a 2560×1600 canvas;
- WKWebView cannot hold 16.7 ms at p95 for 100 curves.

This should be measured before the canvas work package of phase 1 (WP-1.2) starts, since it decides what that package builds.
