# Third-party components

Policy (BUILD-SPEC rule 4): permissive licenses only. No GPL, AGPL, or LGPL. MPL-2.0 is accepted only for
LibreOffice used as an unmodified external program (and for build-time-only transitive tooling).
Every dependency added to the project is recorded here in the same commit.

## Rust (direct)

| Crate | License | Used for |
|---|---|---|
| serde, serde_json, toml | MIT OR Apache-2.0 | serialization |
| tokio, futures, async-trait | MIT (OR Apache-2.0) | async runtime |
| reqwest (rustls) | MIT OR Apache-2.0 | HTTP to model endpoints |
| anyhow, thiserror, tracing | MIT OR Apache-2.0 | errors, logging |
| chrono, uuid, regex, base64, hex, bytes, sha2, walkdir, globset, tempfile, rand | MIT OR Apache-2.0 | utilities |
| clap | MIT OR Apache-2.0 | CLI |
| schemars, ts-rs | MIT | schema and TypeScript generation |
| rusqlite (bundled SQLite) | MIT (SQLite: public domain) | index |
| zstd | MIT (zstd: BSD-3-Clause) | snapshot store |
| serde_yaml_ng | MIT OR Apache-2.0 | skill front matter |
| json-patch | MIT OR Apache-2.0 | RFC 6902 patches on slide documents |
| windows-sys (Windows only) | MIT OR Apache-2.0 | AppContainer, ACLs, job objects in the sandbox helper |
| libc (Unix only) | MIT OR Apache-2.0 | resource limits in the sandbox helper fallback |
| tauri, tauri-build, tauri-plugin-dialog | MIT OR Apache-2.0 | desktop shell, native dialogs (links the system WebView: WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux) |

## JavaScript (desktop renderer)

| Package | License | Used for |
|---|---|---|
| react, react-dom | MIT | UI |
| zustand | MIT | state stores |
| dockview-react | MIT | dockable panel layout |
| @tauri-apps/api | MIT OR Apache-2.0 | calls into the Rust core |
| vite, @vitejs/plugin-react, vitest, jsdom, eslint, typescript-eslint, eslint-plugin-react-hooks, globals (dev only) | MIT | build, tests, lint |
| typescript, @playwright/test, @tauri-apps/cli (dev only) | Apache-2.0 (tauri-cli: MIT OR Apache-2.0) | type checking, browser tests, packaging |

## Python (fixed bundled set, PROTOCOL 1.3)

| Package | License | Used for |
|---|---|---|
| python-pptx | MIT | reading templates, writing .pptx |
| lxml | BSD-3-Clause | XML |
| Pillow | MIT-CMU (HPND) | images |
| openpyxl | MIT | spreadsheets |
| python-docx | MIT | Word documents |
| pypdfium2 (PDFium) | Apache-2.0 OR BSD-3-Clause | PDF text and rendering |
| numpy, pandas | BSD-3-Clause | transforms |
| matplotlib | PSF-based (BSD-compatible) | figures in code mode |
| jsonschema | MIT | deck validation |
| uharfbuzz (HarfBuzz) | Apache-2.0 (MIT) | text shaping for measurement |
| olefile | BSD-2-Clause | Outlook .msg container |
| beautifulsoup4 | MIT | HTML extraction |
| onnxruntime | MIT | bundled embedding model |
| tokenizers | Apache-2.0 | tokenization for embeddings |
| zstandard | BSD-3-Clause | compression |
| pywin32 (Windows only) | PSF | PowerPoint automation |

## External programs (not linked)

| Program | License | Used for |
|---|---|---|
| LibreOffice | MPL-2.0 | optional fidelity rendering, unmodified, separate process |
| Microsoft PowerPoint | proprietary, user-installed | optional fidelity rendering via automation |

## Fonts (tests only, vendor/fonts)

Recorded when added.

| Font | License | Source |
|---|---|---|
| Liberation Sans (Regular, Bold, Italic, Bold Italic) | SIL OFL 1.1 | fonts-liberation |
| Carlito (Regular, Bold, Italic, Bold Italic) | SIL OFL 1.1 | fonts-crosextra-carlito |
| DejaVu Serif (Book, Bold) | Bitstream Vera / DejaVu license (permissive) | fonts-dejavu-core |

The build spec named Noto fonts; the Liberation, Carlito, and DejaVu families are vendored instead because they
are metric-compatible stand-ins for Arial, Calibri, and a serif face, and were available offline.
