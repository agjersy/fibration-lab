# Rendering spike (BUILD-SPEC WP-0.12)

*2026-11-24. Question (MQ-2): can the canvas be WebGL2 inside the system webview, or does it need a native GPU surface? Short answer: **not decided by this spike.** The prototype and the shader pipeline work, but the only renderer available here was a CPU rasterizer, which says nothing reliable about WKWebView or WebKitGTK on a real GPU. The page is ready to run in those webviews; section 5 says how.*

## 1. What was built

- **`code.glsl` in the engine** (`fibration_math_engine/glsl.py`). One scene object becomes GLSL ES 3.00 with the scene's parameters as uniforms, so a slider drag is a uniform update: no recompile and no round trip to the engine. The translation is an allowlist walk that refuses what it cannot represent. Its tests run the generated GLSL in headless Chromium and compare it numerically with the engine.
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

- **macOS (WKWebView's engine):** open `math/spike/render/index.html?auto=1&curves=20&samples=2048&frames=600` in Safari. Repeat with `curves=100` and with `implicit=1`.
- **Linux (WebKitGTK):** open the same URL in GNOME Web (Epiphany), which uses WebKitGTK. Note the renderer string: if it names `llvmpipe` or another software renderer, acceleration is off.
- **Inside a real Tauri window** (the most faithful test): point a minimal Tauri app's window at the same file on each platform.
- **Chromium with a GPU, as a reference:** `node math/spike/render/measure.mjs --gpu`.

To regenerate the scene from the engine: `cd math/python/engine && uv run python ../../spike/render/gen_scene.py`.

## 6. Transformations and 3D (added the same day)

The requirement for grids, vectors and objects under transformations in 2D and 3D (ARCHITECTURE 9.4) changes what the canvas must do, so the prototype was extended before anything is built on it. `spike/render/transforms.html` is **one pipeline that is 3D from the start**; 2D is the case z = 0 with an orthographic camera. Every object is transformed in the vertex shader as `p' = mix(p, T(p), s)`: animating the map or the blend is a uniform update, and no geometry is sent again.

| Scene | What it draws | Draw calls |
|---|---|---|
| `grid2d-linear` | A fixed reference grid, an 82-line grid under a matrix with a blend, 200 vectors and the two basis vectors following it, the unit square | 3 |
| `grid2d-nonlinear` | The same under (x, y) → (x² − y², 2xy) or a swirl, each grid line cut into pieces so it can curve | 3 |
| `grid3d-linear` | A 363-line 3D lattice under a matrix, 300 vectors and three basis vectors with cone heads, the unit cube, a plane, an orbiting perspective camera | 9 |
| `surface3d` | A 96×96 surface z = f(x, y) with a parameter animated, shaded, with iso-lines, over a floor grid | 3 |

All lines in a scene (both grids, axes, vector shafts) are one instanced draw call, and all arrowheads another, so draw calls grow with the kinds of object, not their number. Lines are ribbons with a real width, expanded in the vertex shader and anti-aliased in the fragment shader.

The nonlinear maps and the surface use GLSL generated by the engine. This needed a workaround: the engine has no transformation object yet, so each component of a map was plotted as a separate implicit relation to obtain its shader function. A first-class map with `code.glsl` support is part of WP-0.13.

Measured as before: software rasterizer, 1280×800, 200 frames, milliseconds per frame.

| Configuration | Median | p95 | p99 | Max | Vertices |
|---|---|---|---|---|---|
| `grid2d-linear` | 25.1 | 30.5 | 32.3 | 33.0 | 18,438 |
| `grid2d-nonlinear`, square, 16 pieces per line | 30.7 | 35.6 | 41.7 | 44.6 | 30,948 |
| `grid2d-nonlinear`, square, 64 pieces | 52.9 | 66.0 | 84.0 | 89.8 | 66,084 |
| `grid2d-nonlinear`, swirl, 64 pieces | 56.7 | 68.8 | 80.0 | 81.0 | 66,084 |
| `grid2d-nonlinear`, square, 256 pieces | 143.0 | 249.8 | 268.4 | 268.9 | 206,628 |
| `grid3d-linear`, 4× multisampling | 37.6 | 58.4 | 65.0 | 68.5 | 28,158 |
| `grid3d-linear`, no multisampling | 28.1 | 37.9 | 43.6 | 46.5 | 28,158 |
| `surface3d`, 4× multisampling | 33.3 | 62.6 | 76.6 | 76.9 | 55,696 |
| `surface3d`, no multisampling | 13.4 | 17.7 | 19.1 | 19.5 | 55,696 |

The same caveat applies: these are CPU-rasterizer numbers. What they do show is where the cost goes when the CPU does the work: vertices for tessellated grids, and 10 to 20 ms per frame for multisampling.

**What the pipeline needs from WebGL2.** Everything used is core: GLSL ES 3.00 with `highp` in both stages, `gl_VertexID`, instanced drawing with per-instance attributes, vertex array objects, indexed drawing, depth testing, alpha blending, screen-space derivatives, and a multisampled drawing buffer. No extension is required. `EXT_color_buffer_float` and `EXT_float_blend`, which order-independent transparency would need, are present in this Chromium; whether they are in WKWebView and WebKitGTK is one of the things the real-webview run must report.

**Defects seen in the prototype that the real renderer must handle:**
- Translucent faces are sorted per face. Where two cross (the plane through the cube) no order is right; this is MQ-15.
- Lines do not write depth, so among themselves they draw in submission order. An opaque axis can be over-drawn by a lattice line behind it.
- Ribbons are not clipped against the near plane, so a camera inside the lattice would misdraw them.
- Tessellation is uniform. Strongly curved images show facets near the origin and waste vertices elsewhere; it should follow curvature on screen.
- A vector under a nonlinear map is drawn as the straight segment between the images of its tail and tip. Whether it should instead follow the map's derivative at its tail is a product decision (MQ-19).

**Requirements carried into WP-1.2:** one 3D pipeline with the camera as a view-local uniform; transformations and the blend as uniforms shared by every program; instanced ribbons with per-instance width, colour and transform mode, near-plane clipping and joins; three passes (opaque geometry, opaque lines, then translucent lines and faces); multisampling optional or reduced while animating, since lines are already anti-aliased; vertex budgets chosen from the pixels an object covers.

To run these in the real webviews: open `math/spike/render/transforms.html?scene=grid2d-linear&auto=1` (then `grid2d-nonlinear`, `grid3d-linear`, `surface3d`) in Safari and GNOME Web, or `node math/spike/render/measure-transforms.mjs --gpu` in Chromium with a GPU.

## 7. Recommendation

Stay with the plan of record, **WebGL2 in the webview**, as a working assumption, with **low confidence** until the two webview measurements exist. It keeps docking, undocked windows and the browser test bridge simple, and the renderer needs nothing beyond core WebGL2.

Switch to a native `wgpu` surface if either of these is observed on reference hardware:
- WebKitGTK reports a software renderer, or cannot hold 16.7 ms at p95 for 20 curves on a 2560×1600 canvas;
- WKWebView cannot hold 16.7 ms at p95 for 100 curves.
- either webview cannot hold 16.7 ms at p95 for `grid3d-linear` or for `grid2d-nonlinear` at 64 pieces per line.

This should be measured before the canvas work package of phase 1 (WP-1.2) starts, since it decides what that package builds.
