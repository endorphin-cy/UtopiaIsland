#![allow(dead_code)]

use super::loader::NativePlugin;
use super::types::{
    ContentProvider, Plugin, PluginError, PluginType, ShortcutProvider, ThemeProvider,
};
use super::zip_loader::{self, PluginManifest};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub struct PluginManager {
    entries: RwLock<Vec<NativePlugin>>,
    plugin_dir: PathBuf,
}

impl PluginManager {
    pub fn new<P: AsRef<Path>>(plugin_dir: P) -> Self {
        let plugin_dir = plugin_dir.as_ref().to_path_buf();
        let _ = std::fs::create_dir_all(&plugin_dir);

        Self {
            entries: RwLock::new(Vec::new()),
            plugin_dir,
        }
    }

    pub fn load_all(&self) {
        let dlls = discover_plugins(&self.plugin_dir);
        log::info!(
            "Discovering plugins in {}: {} DLL(s) found",
            self.plugin_dir.display(),
            dlls.len()
        );
        for dll_path in dlls {
            self.load_dll(&dll_path);
        }
    }

    pub(crate) fn load_dll(&self, dll_path: &Path) {
        match NativePlugin::load(dll_path) {
            Ok(native) => {
                let plugin_id = native.metadata().id.clone();

                // C4: reject duplicate plugin IDs
                let entries = match self.entries.read() {
                    Ok(g) => g,
                    Err(_) => {
                        log::error!("Lock poisoned while loading plugin '{}'", plugin_id);
                        return;
                    }
                };
                if entries.iter().any(|p| p.metadata().id == plugin_id) {
                    log::warn!("Plugin '{}' already loaded, skipping duplicate", plugin_id);
                    return;
                }
                drop(entries);

                if let Ok(mut entries) = self.entries.write() {
                    entries.push(native);
                    log::info!(
                        "Loaded plugin: {} ({})",
                        entries.last().unwrap().metadata().name,
                        plugin_id
                    );
                } else {
                    log::error!("Lock poisoned while adding plugin '{}'", plugin_id);
                }
            }
            Err(e) => {
                log::warn!("Failed to load plugin '{}': {}", dll_path.display(), e);
            }
        }
    }

    pub fn install_from_zip(&self, zip_path: &Path) -> Result<PluginManifest, String> {
        let (manifest, _extracted_dir, dll_paths) =
            zip_loader::extract_plugin(zip_path, &self.plugin_dir)?;

        for dll_path in &dll_paths {
            self.load_dll(Path::new(dll_path));
        }

        log::info!(
            "Installed plugin '{}' v{} by {}",
            manifest.name,
            manifest.version,
            manifest.author
        );
        Ok(manifest)
    }

    pub fn read_manifest_from_zip(&self, zip_path: &Path) -> Result<PluginManifest, String> {
        zip_loader::read_manifest_from_zip(zip_path)
    }

    pub fn validate_zip(&self, zip_path: &Path) -> Result<(), String> {
        zip_loader::validate_zip(zip_path)
    }

    pub fn cancel_pending_install(&self, manifest: &PluginManifest) {
        let dir_name = manifest.safe_dir_name();
        let path = self.plugin_dir.join(&dir_name);
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }

    pub fn unload(&self, plugin_id: &str) -> Result<(), PluginError> {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| PluginError::ExecutionError(format!("Lock poisoned: {}", e)))?;
        let idx = entries
            .iter()
            .position(|p| p.metadata().id == plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;
        let plugin = entries.remove(idx);
        log::info!(
            "Plugin unloaded: {} ({})",
            plugin.metadata().name,
            plugin_id
        );
        Ok(())
    }

    pub fn list_content_providers(&self) -> Vec<String> {
        let entries = match self.entries.read() {
            Ok(g) => g,
            Err(e) => {
                log::error!("Plugin lock poisoned: {}", e);
                return Vec::new();
            }
        };
        entries
            .iter()
            .filter(|p| p.plugin_type() == PluginType::Content)
            .map(|p| p.metadata().id.clone())
            .collect()
    }

    pub fn list_theme_providers(&self) -> Vec<String> {
        let entries = match self.entries.read() {
            Ok(g) => g,
            Err(e) => {
                log::error!("Plugin lock poisoned: {}", e);
                return Vec::new();
            }
        };
        entries
            .iter()
            .filter(|p| p.plugin_type() == PluginType::Theme)
            .map(|p| p.metadata().id.clone())
            .collect()
    }

    pub fn list_shortcut_providers(&self) -> Vec<String> {
        let entries = match self.entries.read() {
            Ok(g) => g,
            Err(e) => {
                log::error!("Plugin lock poisoned: {}", e);
                return Vec::new();
            }
        };
        entries
            .iter()
            .filter(|p| p.plugin_type() == PluginType::Shortcut)
            .map(|p| p.metadata().id.clone())
            .collect()
    }

    pub fn with_content<F, R>(&self, plugin_id: &str, f: F) -> Result<R, PluginError>
    where
        F: FnOnce(&dyn ContentProvider) -> R,
    {
        let entries = self
            .entries
            .read()
            .map_err(|e| PluginError::ExecutionError(format!("Lock poisoned: {}", e)))?;
        let entry = entries
            .iter()
            .find(|p| p.metadata().id == plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        if entry.plugin_type() != PluginType::Content {
            return Err(PluginError::InvalidPlugin(format!(
                "Plugin '{}' is not a ContentProvider",
                plugin_id
            )));
        }

        Ok(f(entry))
    }

    pub fn with_content_mut<F, R>(&self, plugin_id: &str, f: F) -> Result<R, PluginError>
    where
        F: FnOnce(&mut dyn ContentProvider) -> R,
    {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| PluginError::ExecutionError(format!("Lock poisoned: {}", e)))?;
        let entry = entries
            .iter_mut()
            .find(|p| p.metadata().id == plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        if entry.plugin_type() != PluginType::Content {
            return Err(PluginError::InvalidPlugin(format!(
                "Plugin '{}' is not a ContentProvider",
                plugin_id
            )));
        }

        Ok(f(entry))
    }

    pub fn with_theme<F, R>(&self, plugin_id: &str, f: F) -> Result<R, PluginError>
    where
        F: FnOnce(&dyn ThemeProvider) -> R,
    {
        let entries = self
            .entries
            .read()
            .map_err(|e| PluginError::ExecutionError(format!("Lock poisoned: {}", e)))?;
        let entry = entries
            .iter()
            .find(|p| p.metadata().id == plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        if entry.plugin_type() != PluginType::Theme {
            return Err(PluginError::InvalidPlugin(format!(
                "Plugin '{}' is not a ThemeProvider",
                plugin_id
            )));
        }

        Ok(f(entry))
    }

    pub fn with_shortcut_mut<F, R>(&self, plugin_id: &str, f: F) -> Result<R, PluginError>
    where
        F: FnOnce(&mut dyn ShortcutProvider) -> R,
    {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| PluginError::ExecutionError(format!("Lock poisoned: {}", e)))?;
        let entry = entries
            .iter_mut()
            .find(|p| p.metadata().id == plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        if entry.plugin_type() != PluginType::Shortcut {
            return Err(PluginError::InvalidPlugin(format!(
                "Plugin '{}' is not a ShortcutProvider",
                plugin_id
            )));
        }

        Ok(f(entry))
    }

    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_default()
            .join("WinIsland")
            .join("plugins");
        Self::new(dir)
    }
}

fn discover_plugins(plugin_dir: &Path) -> Vec<PathBuf> {
    if !plugin_dir.exists() {
        return Vec::new();
    }

    let mut result = Vec::new();
    match std::fs::read_dir(plugin_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Ok(sub) = std::fs::read_dir(&path) {
                        for e in sub.flatten() {
                            let p = e.path();
                            if p.extension().is_some_and(|ext| ext == "dll") {
                                result.push(p);
                            }
                        }
                    }
                } else if path.extension().is_some_and(|ext| ext == "dll") {
                    result.push(path);
                }
            }
        }
        Err(e) => {
            log::warn!(
                "Failed to read plugin directory '{}': {}",
                plugin_dir.display(),
                e
            );
        }
    }
    result
}
