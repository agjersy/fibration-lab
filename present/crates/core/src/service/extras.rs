//! Deck commands of docs/IPC.md that sit beside the main editing loop: template thumbnails
//! and replacement, preview images from the renderer, chart data, and assets.

use super::*;
use base64::Engine;
use fibration_deck_protocol::kernel::LayoutThumbnail;
use fibration_deck_protocol::ui::AssetView;

const ASSET_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "emf", "wmf"];

/// Width and height of a PNG, JPEG, or GIF from its header; `(0, 0)` for anything else.
pub fn image_size(bytes: &[u8]) -> (u32, u32) {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return (be32(&bytes[16..20]), be32(&bytes[20..24]));
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        let le16 = |b: &[u8]| u32::from(u16::from_le_bytes([b[0], b[1]]));
        return (le16(&bytes[6..8]), le16(&bytes[8..10]));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            // Start-of-frame markers carry the size; C4, C8, and CC are not frames.
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u32::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
                let w = u32::from(u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]));
                return (w, h);
            }
            if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            i += 2 + len.max(2);
        }
    }
    (0, 0)
}

fn asset_view(root: &Path, path: &Path) -> Option<AssetView> {
    let bytes = std::fs::read(path).ok()?;
    let (width, height) = image_size(&bytes);
    Some(AssetView {
        path: path
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
        sha256: sha256_hex(&bytes),
        width,
        height,
    })
}

impl DeckService {
    /// One small image per template layout, for the layout picker.
    pub async fn template_thumbnails(&self) -> R<Vec<LayoutThumbnail>> {
        let deck = self.ws.deck().read_deck()?;
        let out = self
            .ws
            .kernel
            .call_value("template.thumbnails", json!({"path": deck.template}))
            .await?;
        serde_json::from_value(out).map_err(|e| IpcError::internal(e.to_string()))
    }

    /// Swap the project's template. Slides keep their content; slides whose layout the new
    /// template lacks are reported by the checks that run afterwards.
    pub fn template_replace(self: &Arc<Self>, template_abs: String) -> String {
        self.spawn_task("Replacing the template", move |rt| async move {
            let source = PathBuf::from(&template_abs);
            let ext = source
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !matches!(ext.as_str(), "pptx" | "potx") {
                return Err(IpcError::invalid(
                    "the template must be a .pptx or .potx file",
                ));
            }
            let name = source
                .file_name()
                .ok_or_else(|| IpcError::invalid("the template has no file name"))?
                .to_string_lossy()
                .into_owned();
            let rel = format!("template/{name}");
            let target = rt.ws.project.root.join(&rel);
            let bytes = std::fs::read(&source)
                .map_err(|e| err(IpcErrorCode::Io, format!("{template_abs}: {e}")))?;
            // Inspect a copy first so a broken file never replaces a working template.
            let probe_rel = format!(".present/cache/template-probe.{ext}");
            write_atomic(&rt.ws.project.root.join(&probe_rel), &bytes)
                .map_err(|e| err(IpcErrorCode::Io, e))?;
            let description = rt
                .ws
                .kernel
                .call_value("template.inspect", json!({"path": probe_rel}))
                .await?;
            let _ = std::fs::remove_file(rt.ws.project.root.join(&probe_rel));
            rt.host
                .checkpoint("before replacing the template")
                .map_err(internal)?;
            write_atomic(&target, &bytes).map_err(|e| err(IpcErrorCode::Io, e))?;
            let store = rt.ws.deck();
            let mut deck = store.read_deck()?;
            deck.template = rel;
            store.write_deck(&deck)?;
            {
                let mut st = rt.lock();
                st.tool_state_template = Some(description);
                st.thumbs.clear();
                st.previews.clear();
            }
            let all: Vec<String> = deck.slide_ids().map(|s| s.to_string()).collect();
            rt.deck_changed(vec![], vec![], "user");
            // A slide whose layout the new template lacks cannot be rendered; say so rather
            // than failing the replacement.
            let outcome = match rt.run_checks(None).await {
                Ok(diags) => {
                    let errors = diags.iter().filter(|d| d["severity"] == "error").count();
                    if errors == 0 {
                        format!("{} slides now use {name}", all.len())
                    } else {
                        format!("{name} is in place; {errors} findings need attention")
                    }
                }
                Err(e) => format!(
                    "{name} is in place, but the deck does not build with it: {}",
                    e.message
                ),
            };
            if let Ok(info) = rt.info().await {
                rt.emit(DeckEvent::InfoChanged {
                    info: Box::new(info),
                });
            }
            Ok(Some(outcome))
        })
    }

    /// The renderer's rasterized preview of a slide, used when no rendering engine exists.
    pub fn slide_preview_png(&self, slide_id: &str, png_base64: &str) -> R<()> {
        let id: SlideId = slide_id
            .parse()
            .map_err(|_| IpcError::invalid("not a slide id"))?;
        let hash = self.ws.deck().slide_hash(&id)?;
        let data = png_base64.rsplit(',').next().unwrap_or(png_base64);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|_| IpcError::invalid("the image is not valid base64"))?;
        if !bytes.starts_with(b"\x89PNG") || bytes.len() > 20 * 1024 * 1024 {
            return Err(IpcError::invalid(
                "the image must be a PNG of at most 20 MB",
            ));
        }
        let rel = format!(".present/cache/thumbs/preview/{slide_id}.png");
        write_atomic(&self.ws.project.root.join(&rel), &bytes)
            .map_err(|e| err(IpcErrorCode::Io, e))?;
        self.lock()
            .previews
            .insert(slide_id.to_string(), (rel, hash));
        self.emit_views(&[slide_id.to_string()]);
        Ok(())
    }

    /// Run a chart's declarative transforms and return the resulting table.
    pub async fn chart_compute(&self, source: Value, steps: Value) -> R<Value> {
        let steps = if steps.is_null() { json!([]) } else { steps };
        let mut out = self
            .ws
            .kernel
            .call_value("chart.compute", json!({"source": source, "steps": steps}))
            .await?;
        Ok(out["table"].take())
    }

    /// Images under `assets/` that slides can place.
    pub fn assets_list(&self) -> R<Vec<AssetView>> {
        let root = &self.ws.project.root;
        let mut out = Vec::new();
        let mut stack = vec![root.join("assets")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| {
                    ASSET_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str())
                }) {
                    out.extend(asset_view(root, &path));
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Copy an image into `assets/`. The same bytes under the same name are stored once; a
    /// different image with a taken name gets a short hash suffix.
    pub fn asset_add(&self, path_abs: &str) -> R<AssetView> {
        let source = Path::new(path_abs);
        let ext = source
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !ASSET_EXTENSIONS.contains(&ext.as_str()) {
            return Err(IpcError::invalid(format!(
                "assets must be one of: {}",
                ASSET_EXTENSIONS.join(", ")
            )));
        }
        let bytes =
            std::fs::read(source).map_err(|e| err(IpcErrorCode::Io, format!("{path_abs}: {e}")))?;
        let sha = sha256_hex(&bytes);
        let stem: String = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into())
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let root = &self.ws.project.root;
        let mut target = root.join("assets").join(format!("{stem}.{ext}"));
        if target.is_file()
            && std::fs::read(&target)
                .map(|b| sha256_hex(&b))
                .ok()
                .as_deref()
                != Some(sha.as_str())
        {
            target = root
                .join("assets")
                .join(format!("{stem}-{}.{ext}", &sha[..8]));
        }
        if !target.is_file() {
            write_atomic(&target, &bytes).map_err(|e| err(IpcErrorCode::Io, e))?;
        }
        asset_view(root, &target)
            .ok_or_else(|| IpcError::internal("the asset could not be read back"))
    }
}

#[cfg(test)]
mod tests {
    use super::image_size;

    #[test]
    fn image_sizes_from_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_size(&png), (640, 480));
        let gif = b"GIF89a\x10\x00\x20\x00rest";
        assert_eq!(image_size(gif), (16, 32));
        // SOI, APP0 (length 4), SOF0 with height 100 and width 200.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8, 0, 100, 0, 200, 3, 0, 0,
        ];
        assert_eq!(image_size(&jpeg), (200, 100));
        assert_eq!(image_size(b"not an image"), (0, 0));
    }
}
