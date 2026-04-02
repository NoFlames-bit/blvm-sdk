//! Module Registry
//!
//! High-level module registry API for discovering, installing, updating,
//! and removing modules. Wraps blvm-node module registry functionality.

use crate::composition::types::*;
use blvm_node::module::registry::{
    ModuleDependencies as RefModuleDependencies, ModuleDiscovery as RefModuleDiscovery,
};
use blvm_node::module::traits::ModuleError as RefModuleError;
use std::fs;
use std::path::{Path, PathBuf};

const SOURCE_FILE: &str = ".blvm-source.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct ModuleSourceFile {
    source: String, // "registry" | "git"
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

fn write_source_file(dir: &Path, source: &str, url: &str) -> Result<()> {
    let path = dir.join(SOURCE_FILE);
    let content = ModuleSourceFile {
        source: source.to_string(),
        url: url.to_string(),
        tag: None,
    };
    let json = serde_json::to_string_pretty(&content)
        .map_err(|e| CompositionError::SerializationError(e.to_string()))?;
    fs::write(&path, json).map_err(CompositionError::IoError)?;
    Ok(())
}

fn write_source_file_git(dir: &Path, url: &str, tag: Option<&str>) -> Result<()> {
    let path = dir.join(SOURCE_FILE);
    let content = ModuleSourceFile {
        source: "git".to_string(),
        url: url.to_string(),
        tag: tag.map(String::from),
    };
    let json = serde_json::to_string_pretty(&content)
        .map_err(|e| CompositionError::SerializationError(e.to_string()))?;
    fs::write(&path, json).map_err(CompositionError::IoError)?;
    Ok(())
}

fn read_source_file(dir: &Path) -> Result<Option<ModuleSourceFile>> {
    let path = dir.join(SOURCE_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(CompositionError::IoError)?;
    let parsed: ModuleSourceFile = serde_json::from_str(&content)
        .map_err(|e| CompositionError::SerializationError(e.to_string()))?;
    Ok(Some(parsed))
}

/// Module registry for managing module lifecycle
pub struct ModuleRegistry {
    /// Base directory for modules
    modules_dir: PathBuf,
    /// Discovered modules cache
    discovered: Vec<ModuleInfo>,
}

impl ModuleRegistry {
    /// Create a new module registry
    pub fn new<P: AsRef<Path>>(modules_dir: P) -> Self {
        Self {
            modules_dir: modules_dir.as_ref().to_path_buf(),
            discovered: Vec::new(),
        }
    }

    /// Discover available modules in the modules directory
    pub fn discover_modules(&mut self) -> Result<Vec<ModuleInfo>> {
        let discovery = RefModuleDiscovery::new(&self.modules_dir);
        let discovered = discovery
            .discover_modules()
            .map_err(|e: RefModuleError| CompositionError::from(e))?;

        self.discovered = discovered.iter().map(|d| ModuleInfo::from(d)).collect();

        Ok(self.discovered.clone())
    }

    /// Get module by name and optional version
    pub fn get_module(&self, name: &str, version: Option<&str>) -> Result<ModuleInfo> {
        let module = self
            .discovered
            .iter()
            .find(|m| m.name == name && version.map_or(true, |v| m.version == v))
            .ok_or_else(|| {
                let msg = if let Some(v) = version {
                    format!("Module {} version {} not found", name, v)
                } else {
                    format!("Module {} not found", name)
                };
                CompositionError::ModuleNotFound(msg)
            })?;

        Ok(module.clone())
    }

    /// Install module from source
    pub fn install_module(&mut self, source: ModuleSource) -> Result<ModuleInfo> {
        match source {
            ModuleSource::Path(path) => {
                // Validate path exists
                if !path.exists() {
                    return Err(CompositionError::InstallationFailed(format!(
                        "Module path does not exist: {:?}",
                        path
                    )));
                }

                // For now, we'll just discover from the path
                // In a full implementation, this would copy/install the module
                let discovery = RefModuleDiscovery::new(&path);
                let discovered = discovery
                    .discover_modules()
                    .map_err(|e| CompositionError::from(e))?;

                if discovered.is_empty() {
                    return Err(CompositionError::InstallationFailed(
                        "No module found at path".to_string(),
                    ));
                }

                // Refresh discovered modules
                self.discover_modules()?;

                Ok(ModuleInfo::from(&discovered[0]))
            }
            ModuleSource::Registry { url, name } => {
                self.install_from_registry(&url, name.as_deref())
            }
            ModuleSource::Git { url, tag } => self.install_from_git(&url, tag.as_deref()),
        }
    }

    /// Update module to new version (re-pull from git if from git, else re-download from registry)
    pub fn update_module(&mut self, name: &str, _new_version: Option<&str>) -> Result<ModuleInfo> {
        let current = self.get_module(name, None)?;
        let dir = current.directory.as_ref().ok_or_else(|| {
            CompositionError::InstallationFailed("Module has no directory".to_string())
        })?;

        if let Some(source_file) = read_source_file(dir)? {
            match source_file.source.as_str() {
                "git" => {
                    #[cfg(feature = "git")]
                    {
                        self.update_module_from_git(name, new_version)?;
                        return self.get_module(name, new_version);
                    }
                    #[cfg(not(feature = "git"))]
                    {
                        return Err(CompositionError::InstallationFailed(
                            "Module update from git requires 'git' feature".to_string(),
                        ));
                    }
                }
                "registry" => {
                    #[cfg(feature = "registry")]
                    {
                        self.remove_module(name)?;
                        self.install_from_registry(&source_file.url, Some(name))?;
                        return self.get_module(name, new_version);
                    }
                    #[cfg(not(feature = "registry"))]
                    {
                        return Err(CompositionError::InstallationFailed(
                            "Module update from registry requires 'registry' feature".to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }

        // Fallback: try git if .git exists (legacy installs without .blvm-source.json)
        let git_dir = dir.join(".git");
        if git_dir.exists() {
            #[cfg(feature = "git")]
            {
                self.update_module_from_git(name, new_version)?;
                return self.get_module(name, new_version);
            }
        }

        Err(CompositionError::InstallationFailed(
            "Module has no install source (.blvm-source.json). Reinstall from registry or git."
                .to_string(),
        ))
    }

    #[cfg(feature = "registry")]
    fn install_from_registry(&mut self, url: &str, name: Option<&str>) -> Result<ModuleInfo> {
        let index: serde_json::Value = reqwest::blocking::get(url)
            .map_err(|e| {
                CompositionError::InstallationFailed(format!("Registry fetch failed: {}", e))
            })?
            .json()
            .map_err(|e| {
                CompositionError::InstallationFailed(format!("Registry JSON parse failed: {}", e))
            })?;

        let modules = index
            .get("modules")
            .and_then(|m| m.as_array())
            .ok_or_else(|| {
                CompositionError::InstallationFailed("Registry missing 'modules' array".to_string())
            })?;

        if modules.is_empty() {
            return Err(CompositionError::InstallationFailed(
                "Registry has no modules".to_string(),
            ));
        }

        let selected = if let Some(n) = name {
            modules
                .iter()
                .find(|m| m.get("name").and_then(|v| v.as_str()) == Some(n))
                .ok_or_else(|| {
                    CompositionError::InstallationFailed(format!(
                        "Module '{}' not found in registry",
                        n
                    ))
                })?
        } else {
            &modules[0]
        };

        let first = selected;
        let name = first.get("name").and_then(|n| n.as_str()).ok_or_else(|| {
            CompositionError::InstallationFailed("Module missing 'name'".to_string())
        })?;
        let download_url = first
            .get("download_url")
            .or_else(|| first.get("url"))
            .and_then(|u| u.as_str())
            .ok_or_else(|| {
                CompositionError::InstallationFailed("Module missing download_url".to_string())
            })?;

        let bytes = reqwest::blocking::get(download_url)
            .map_err(|e| CompositionError::InstallationFailed(format!("Download failed: {}", e)))?
            .bytes()
            .map_err(|e| {
                CompositionError::InstallationFailed(format!("Download read failed: {}", e))
            })?;

        let dest_dir = self.modules_dir.join(name);
        fs::create_dir_all(&dest_dir)?;
        let archive_path = dest_dir.join("module.tar.gz");
        fs::write(&archive_path, &bytes).map_err(CompositionError::IoError)?;

        // Extract tar.gz using system tar (no extra deps)
        let status = std::process::Command::new("tar")
            .args([
                "-xzf",
                archive_path.to_str().unwrap(),
                "-C",
                dest_dir.to_str().unwrap(),
            ])
            .status()
            .map_err(|e| {
                CompositionError::InstallationFailed(format!("tar extraction failed: {}", e))
            })?;
        fs::remove_file(&archive_path).ok();
        if !status.success() {
            return Err(CompositionError::InstallationFailed(
                "tar extraction failed".to_string(),
            ));
        }

        self.discover_modules()?;
        let info = self.get_module(name, None)?;
        let dir = info
            .directory
            .as_ref()
            .unwrap_or(&self.modules_dir.join(name));
        write_source_file(dir, "registry", url)?;
        Ok(info)
    }

    #[cfg(not(feature = "registry"))]
    fn install_from_registry(&mut self, _url: &str, _name: Option<&str>) -> Result<ModuleInfo> {
        Err(CompositionError::InstallationFailed(
            "Registry installation requires 'registry' feature (reqwest)".to_string(),
        ))
    }

    #[cfg(feature = "git")]
    fn install_from_git(&mut self, url: &str, tag: Option<&str>) -> Result<ModuleInfo> {
        let repo_name = url
            .split('/')
            .last()
            .unwrap_or("module")
            .trim_end_matches(".git");
        let dest_dir = self.modules_dir.join(repo_name);

        if dest_dir.exists() {
            fs::remove_dir_all(&dest_dir).map_err(CompositionError::IoError)?;
        }

        let mut builder = git2::build::RepoBuilder::new();
        if let Some(t) = tag {
            builder.branch(t);
        }
        builder.clone(url, &dest_dir).map_err(|e| {
            CompositionError::InstallationFailed(format!("Git clone failed: {}", e))
        })?;

        write_source_file_git(&dest_dir, url, tag)?;
        self.discover_modules()?;
        self.get_module(repo_name, None)
    }

    #[cfg(not(feature = "git"))]
    fn install_from_git(&mut self, _url: &str, _tag: Option<&str>) -> Result<ModuleInfo> {
        Err(CompositionError::InstallationFailed(
            "Git installation requires 'git' feature (git2)".to_string(),
        ))
    }

    #[cfg(feature = "git")]
    fn update_module_from_git(&mut self, name: &str, _new_version: Option<&str>) -> Result<()> {
        let current = self.get_module(name, None)?;
        let dir = current.directory.as_ref().ok_or_else(|| {
            CompositionError::InstallationFailed("Module has no directory".to_string())
        })?;

        let repo = git2::Repository::open(dir)
            .map_err(|e| CompositionError::InstallationFailed(format!("Git open failed: {}", e)))?;
        let mut remote = repo.find_remote("origin").map_err(|e| {
            CompositionError::InstallationFailed(format!("Git remote origin not found: {}", e))
        })?;
        remote.fetch(&[], None, None).map_err(|e| {
            CompositionError::InstallationFailed(format!("Git fetch failed: {}", e))
        })?;

        let fetch_head = repo.find_reference("FETCH_HEAD").map_err(|e| {
            CompositionError::InstallationFailed(format!("FETCH_HEAD failed: {}", e))
        })?;
        let oid = fetch_head.target().ok_or_else(|| {
            CompositionError::InstallationFailed("Invalid FETCH_HEAD".to_string())
        })?;
        let obj = repo.find_object(oid, None).map_err(|e| {
            CompositionError::InstallationFailed(format!("Find object failed: {}", e))
        })?;
        repo.checkout_tree(&obj, None).map_err(|e| {
            CompositionError::InstallationFailed(format!("Checkout tree failed: {}", e))
        })?;
        repo.set_head_detached(oid)
            .map_err(|e| CompositionError::InstallationFailed(format!("Checkout failed: {}", e)))?;

        self.discover_modules()?;
        Ok(())
    }

    /// Remove module from disk.
    /// Callers with a running node should stop the module first via `ModuleLifecycle::stop_module`.
    pub fn remove_module(&mut self, name: &str) -> Result<()> {
        let module = self.get_module(name, None)?;

        if let Some(dir) = &module.directory {
            std::fs::remove_dir_all(dir).map_err(CompositionError::IoError)?;
        }

        // Refresh discovered modules
        self.discover_modules()?;

        Ok(())
    }

    /// List all installed modules
    pub fn list_modules(&self) -> Vec<ModuleInfo> {
        self.discovered.clone()
    }

    /// Resolve dependencies for a set of modules
    pub fn resolve_dependencies(&self, module_names: &[String]) -> Result<Vec<ModuleInfo>> {
        // First, we need to get the actual RefDiscoveredModule objects
        // We'll need to re-discover or cache them. For now, let's re-discover.
        let discovery = RefModuleDiscovery::new(&self.modules_dir);
        let all_discovered = discovery
            .discover_modules()
            .map_err(|e| CompositionError::from(e))?;

        // Filter to only requested modules and convert to owned values
        let requested: Vec<_> = all_discovered
            .iter()
            .filter(|d| module_names.contains(&d.manifest.name))
            .cloned()
            .collect();

        let resolution =
            RefModuleDependencies::resolve(&requested).map_err(|e| CompositionError::from(e))?;

        // Build result with resolved modules
        let mut resolved = Vec::new();
        for name in &resolution.load_order {
            let module = self.get_module(name, None)?;
            resolved.push(module);
        }

        Ok(resolved)
    }
}
