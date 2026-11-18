//! Commands of docs/IPC.md that sit beside the main editing loop: template thumbnails and
//! replacement, preview images from the renderer, chart data, Personal Analyst imports,
//! assets, and provider keys.

use super::*;
use analyst_deck_protocol::kernel::LayoutThumbnail;
use analyst_deck_protocol::ui::{AnalystItem, AssetView};
use base64::Engine;

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

impl Workbench {
    /// Store a provider's API key in the home `.env` file (mode 0600) under the variable the
    /// provider's `key = "env:<VAR>"` names. The key is never returned or logged. A value
    /// for the same variable in the process environment still takes precedence.
    pub fn provider_set_key(&self, provider: &str, key: &str) -> R<()> {
        let global = self.global();
        let cfg = global
            .providers
            .iter()
            .find(|p| p.name == provider)
            .ok_or_else(|| {
                IpcError::not_found(format!("no provider named {provider:?} in config.toml"))
            })?;
        let Some(reference) = cfg.key.as_deref() else {
            return Err(IpcError::invalid(format!(
                "provider {provider:?} has no key entry in config.toml; add key = \"env:<VAR>\" to name the variable its key is stored under"
            )));
        };
        let reference = wb_providers::keys::KeyRef::parse(reference)
            .map_err(|e| IpcError::invalid(format!("provider {provider:?}: {e}")))?;
        if key.trim().is_empty() {
            return Err(IpcError::invalid("the key is empty"));
        }
        wb_providers::keys::KeyStores::for_home(&self.home)
            .set(&reference, key.trim())
            .map_err(|e| IpcError::invalid(format!("the key could not be stored: {e}")))
    }
}

impl Workbench {
    /// Close every open project (app exit).
    pub async fn shutdown(&self) {
        let ids: Vec<String> = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for id in ids {
            let _ = self.project_close(&id).await;
        }
    }
}

impl ProjectRuntime {
    /// Write a policy change into the project's `project.toml` so it outlives the session.
    /// The file is rewritten from its parsed form: values are kept, comments are not.
    pub fn persist_policy(&self, patch: &PolicyPatch) -> R<()> {
        let path = self.ws.project.root.join(".present").join("project.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(err(IpcErrorCode::Io, e)),
        };
        let mut doc: toml::Table = text
            .parse()
            .map_err(|e| IpcError::invalid(format!("project.toml: {e}")))?;
        let policy = doc
            .entry("policy")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| IpcError::invalid("project.toml: [policy] is not a table"))?;
        for (key, value) in [
            ("edits", &patch.edits),
            ("code", &patch.code),
            ("skills_scripts", &patch.skills_scripts),
            ("code_mode", &patch.code_mode),
            ("export", &patch.export),
        ] {
            if let Some(v) = value {
                policy.insert(key.into(), toml::Value::String(v.clone()));
            }
        }
        if let Some(n) = patch.max_steps {
            policy.insert("max_steps".into(), toml::Value::Integer(i64::from(n)));
        }
        let updated =
            toml::to_string_pretty(&doc).map_err(|e| IpcError::internal(e.to_string()))?;
        // Refuse values the configuration does not accept before anything is written.
        toml::from_str::<analyst_deck_protocol::config::ProjectConfig>(&updated)
            .map_err(|e| IpcError::invalid(format!("not a valid policy: {e}")))?;
        write_atomic(&path, updated.as_bytes()).map_err(|e| err(IpcErrorCode::Io, e))
    }

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
            rt.ws
                .snapshot(
                    analyst_deck_protocol::config::SnapshotReason::Manual,
                    None,
                    Some("before replacing the template"),
                )
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
                rt.emit(ProjectEvent::ProjectChanged {
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

    /// Figures and tables a Personal Analyst repository offers for import.
    pub async fn analyst_scan(&self, repo_abs: &str) -> R<Vec<AnalystItem>> {
        let out = self
            .ws
            .kernel
            .call_value("analyst.scan", json!({"repo_abs": repo_abs}))
            .await?;
        let mut items = Vec::new();
        for (kind, key) in [("figure", "figures"), ("table", "tables")] {
            for v in out[key].as_array().into_iter().flatten() {
                let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                let id = text("item");
                items.push(AnalystItem {
                    thumb: (kind == "figure" && v["png"] == true)
                        .then(|| Path::new(repo_abs).join(&id).to_string_lossy().into_owned()),
                    id,
                    kind: kind.into(),
                    title: text("title"),
                    run_id: text("run_id"),
                    script: text("script"),
                    args: v["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect(),
                    created: text("created"),
                });
            }
        }
        Ok(items)
    }

    /// Copy one Analyst figure or table into the project as a reference.
    pub async fn analyst_import(&self, repo_abs: &str, item: &str) -> R<RefView> {
        let entry = self
            .ws
            .add_analyst_reference(repo_abs, item)
            .await
            .map_err(|e| IpcError::invalid(format!("{e:#}")))?;
        let refs = self.refs_list()?;
        let view = refs
            .iter()
            .find(|r| r.id == entry.id.as_str())
            .cloned()
            .ok_or_else(|| IpcError::internal("the imported reference is not listed"))?;
        self.emit(ProjectEvent::RefsChanged { refs });
        Ok(view)
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
