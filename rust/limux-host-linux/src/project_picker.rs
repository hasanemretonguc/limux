use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const RECENT_PROJECTS_FILE: &str = "recent-projects.json";
const MAX_RECENT_PROJECTS: usize = 24;
const PROJECT_ROOT_NAMES: &[&str] = &["Projects", "projects", "dev", "Code", "src", "workspace"];

pub const PROJECT_PICKER_CSS: &str = r#"
.limux-project-list {
    background: transparent;
}
.limux-project-list row {
    padding: 4px 6px;
    border-radius: 4px;
    margin: 1px 0;
}
.limux-project-list row:hover {
    background: alpha(@window_fg_color, 0.06);
}
.limux-project-list row:selected {
    background: alpha(@accent_bg_color, 0.16);
}
.limux-project-badge {
    font-size: 10px;
    font-weight: 700;
    min-width: 28px;
    min-height: 22px;
    padding: 0 5px;
    border-radius: 3px;
    background: alpha(@window_fg_color, 0.10);
    color: @window_fg_color;
}
.limux-project-badge-rust {
    background: alpha(#dea584, 0.22);
    color: #dea584;
}
.limux-project-badge-flutter {
    background: alpha(#54c5f8, 0.22);
    color: #54c5f8;
}
.limux-project-badge-typescript {
    background: alpha(#3178c6, 0.24);
    color: #69b2e8;
}
.limux-project-badge-node {
    background: alpha(#68a063, 0.22);
    color: #84c97e;
}
.limux-project-badge-python {
    background: alpha(#ffd43b, 0.20);
    color: #ffd43b;
}
.limux-project-badge-go {
    background: alpha(#00add8, 0.22);
    color: #4dd0e1;
}
.limux-project-badge-dotnet {
    background: alpha(#8b5cf6, 0.24);
    color: #a78bfa;
}
.limux-project-badge-java {
    background: alpha(#f89820, 0.22);
    color: #f89820;
}
.limux-project-badge-zig {
    background: alpha(#f7a41d, 0.22);
    color: #f7a41d;
}
.limux-project-badge-cpp {
    background: alpha(#64748b, 0.24);
    color: #94a3b8;
}
.limux-project-badge-git {
    background: alpha(#f05032, 0.20);
    color: #f05032;
}
.limux-project-name {
    font-weight: 600;
    font-size: 13px;
}
.limux-project-path {
    font-size: 11px;
    color: alpha(@window_fg_color, 0.50);
}
.limux-project-tag {
    font-size: 10px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 3px;
    background: alpha(@window_fg_color, 0.08);
    color: alpha(@window_fg_color, 0.65);
}
.limux-project-tag-recent {
    background: alpha(@accent_bg_color, 0.18);
    color: @accent_color;
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    Rust,
    Flutter,
    TypeScript,
    Node,
    Python,
    Go,
    DotNet,
    Java,
    Zig,
    Cpp,
    Git,
    Folder,
}

impl ProjectKind {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Rust => "RS",
            Self::Flutter => "FL",
            Self::TypeScript => "TS",
            Self::Node => "JS",
            Self::Python => "PY",
            Self::Go => "GO",
            Self::DotNet => "C#",
            Self::Java => "JV",
            Self::Zig => "ZG",
            Self::Cpp => "C+",
            Self::Git => "GIT",
            Self::Folder => "DIR",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Flutter => "Flutter",
            Self::TypeScript => "TypeScript",
            Self::Node => "Node.js",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::DotNet => ".NET",
            Self::Java => "Java",
            Self::Zig => "Zig",
            Self::Cpp => "C/C++",
            Self::Git => "Git",
            Self::Folder => "Folder",
        }
    }

    pub fn badge_css_class(self) -> Option<&'static str> {
        match self {
            Self::Rust => Some("limux-project-badge-rust"),
            Self::Flutter => Some("limux-project-badge-flutter"),
            Self::TypeScript => Some("limux-project-badge-typescript"),
            Self::Node => Some("limux-project-badge-node"),
            Self::Python => Some("limux-project-badge-python"),
            Self::Go => Some("limux-project-badge-go"),
            Self::DotNet => Some("limux-project-badge-dotnet"),
            Self::Java => Some("limux-project-badge-java"),
            Self::Zig => Some("limux-project-badge-zig"),
            Self::Cpp => Some("limux-project-badge-cpp"),
            Self::Git => Some("limux-project-badge-git"),
            Self::Folder => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub name: String,
    pub path: PathBuf,
    pub display_path: String,
    pub kind: ProjectKind,
    pub is_recent: bool,
    pub is_open: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecentProjectsFile {
    #[serde(default)]
    entries: Vec<RecentProjectRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecentProjectRecord {
    path: String,
    last_opened_unix: u64,
}

pub fn detect_project_kind(dir: &Path) -> ProjectKind {
    if dir.join("Cargo.toml").is_file() {
        return ProjectKind::Rust;
    }
    if dir.join("pubspec.yaml").is_file() {
        return ProjectKind::Flutter;
    }
    if dir.join("tsconfig.json").is_file() {
        return ProjectKind::TypeScript;
    }
    if dir.join("package.json").is_file() {
        return ProjectKind::Node;
    }
    if dir.join("pyproject.toml").is_file()
        || dir.join("requirements.txt").is_file()
        || dir.join("setup.py").is_file()
    {
        return ProjectKind::Python;
    }
    if dir.join("go.mod").is_file() {
        return ProjectKind::Go;
    }
    if dir.join("build.zig").is_file() {
        return ProjectKind::Zig;
    }
    if dir.join("pom.xml").is_file()
        || dir.join("build.gradle").is_file()
        || dir.join("build.gradle.kts").is_file()
    {
        return ProjectKind::Java;
    }
    if dir.join("CMakeLists.txt").is_file() || dir.join("Makefile").is_file() {
        return ProjectKind::Cpp;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".csproj") || name.ends_with(".sln") || name.ends_with(".fsproj") {
                return ProjectKind::DotNet;
            }
        }
    }
    if dir.join(".git").exists() {
        return ProjectKind::Git;
    }
    ProjectKind::Folder
}

fn recent_projects_path() -> Option<PathBuf> {
    let state = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .or_else(dirs::data_local_dir)?;
    Some(state.join("limux").join(RECENT_PROJECTS_FILE))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn record_recent_project(path: &Path) {
    let Some(store_path) = recent_projects_path() else {
        return;
    };
    let _ = record_recent_project_in(&store_path, path, now_unix_secs());
}

fn record_recent_project_in(
    store_path: &Path,
    project_path: &Path,
    now: u64,
) -> Result<(), String> {
    let normalized = fs::canonicalize(project_path).unwrap_or_else(|_| project_path.to_path_buf());
    let path_str = normalized.to_string_lossy().to_string();
    if path_str.is_empty() {
        return Ok(());
    }

    let mut store = load_recent_file(store_path);
    store.entries.retain(|entry| entry.path != path_str);
    store.entries.insert(
        0,
        RecentProjectRecord {
            path: path_str,
            last_opened_unix: now,
        },
    );
    store.entries.truncate(MAX_RECENT_PROJECTS);

    if let Some(parent) = store_path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let serialized = serde_json::to_string_pretty(&store).map_err(|err| err.to_string())?;
    let tmp_path = store_path.with_extension("json.tmp");
    fs::write(&tmp_path, serialized).map_err(|err| err.to_string())?;
    fs::rename(&tmp_path, store_path).map_err(|err| err.to_string())?;
    Ok(())
}

fn load_recent_file(store_path: &Path) -> RecentProjectsFile {
    fs::read_to_string(store_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<RecentProjectsFile>(&raw).ok())
        .unwrap_or_default()
}

pub fn shorten_display_path(path: &Path, home_dir: Option<&Path>) -> String {
    if let Some(home) = home_dir {
        if let Ok(rel) = path.strip_prefix(home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.display());
        }
    }
    path.to_string_lossy().to_string()
}

pub fn collect_project_entries(open_workspace_paths: &[String]) -> Vec<ProjectEntry> {
    let home = dirs::home_dir();
    let recent_paths: Vec<PathBuf> = recent_projects_path()
        .map(|p| {
            load_recent_file(&p)
                .entries
                .into_iter()
                .map(|r| PathBuf::from(r.path))
                .collect()
        })
        .unwrap_or_default();
    collect_project_entries_with_inputs(open_workspace_paths, &recent_paths, home.as_deref())
}

fn collect_project_entries_with_inputs(
    open_workspace_paths: &[String],
    recent_paths: &[PathBuf],
    home_dir: Option<&Path>,
) -> Vec<ProjectEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    let open_set: HashSet<PathBuf> = open_workspace_paths
        .iter()
        .map(|p| {
            let pb = PathBuf::from(p);
            fs::canonicalize(&pb).unwrap_or(pb)
        })
        .collect();

    let mut push_path = |raw: &Path, is_recent: bool| {
        if !raw.is_dir() {
            return;
        }
        let canonical = fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf());
        if !seen.insert(canonical.clone()) {
            return;
        }
        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| canonical.to_string_lossy().to_string());
        let display_path = shorten_display_path(&canonical, home_dir);
        let kind = detect_project_kind(&canonical);
        let is_open = open_set.contains(&canonical);
        entries.push(ProjectEntry {
            name,
            path: canonical,
            display_path,
            kind,
            is_recent: is_recent || is_open,
            is_open,
        });
    };

    // 1. Recent persisted locations first (most recently opened at top)
    for path in recent_paths {
        push_path(path, true);
    }

    // 2. Currently open workspace folders not yet in recent-projects.json
    for raw in open_workspace_paths {
        push_path(Path::new(raw), true);
    }

    // 3. Auto-discovered projects under ~/Projects, ~/dev, ~/Code, etc.
    let mut discovered = Vec::new();
    if let Some(home) = home_dir {
        for root_name in PROJECT_ROOT_NAMES {
            let root = home.join(root_name);
            let Ok(read_dir) = fs::read_dir(&root) else {
                continue;
            };
            for child in read_dir.flatten() {
                let child_path = child.path();
                let is_hidden = child
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with('.'));
                if !is_hidden && child_path.is_dir() {
                    discovered.push(child_path);
                }
            }
        }
    }
    discovered.sort_by(|a, b| {
        a.file_name()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .cmp(
                &b.file_name()
                    .map(|s| s.to_string_lossy().to_ascii_lowercase()),
            )
    });
    for path in discovered {
        push_path(&path, false);
    }

    entries
}

pub fn filter_projects<'a>(entries: &'a [ProjectEntry], query: &str) -> Vec<&'a ProjectEntry> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .filter(|entry| {
            entry.name.to_ascii_lowercase().contains(&needle)
                || entry.display_path.to_ascii_lowercase().contains(&needle)
                || entry.kind.label().to_ascii_lowercase().contains(&needle)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        collect_project_entries_with_inputs, detect_project_kind, filter_projects,
        load_recent_file, record_recent_project_in, shorten_display_path, ProjectKind,
    };
    use std::fs;
    use std::path::Path;

    #[test]
    fn detects_common_project_kinds() {
        let dir = tempfile::tempdir().unwrap();

        let rust_dir = dir.path().join("rust-app");
        fs::create_dir_all(&rust_dir).unwrap();
        fs::write(rust_dir.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        assert_eq!(detect_project_kind(&rust_dir), ProjectKind::Rust);

        let flutter_dir = dir.path().join("flutter-app");
        fs::create_dir_all(&flutter_dir).unwrap();
        fs::write(flutter_dir.join("pubspec.yaml"), "name: app\n").unwrap();
        assert_eq!(detect_project_kind(&flutter_dir), ProjectKind::Flutter);

        let ts_dir = dir.path().join("ts-app");
        fs::create_dir_all(&ts_dir).unwrap();
        fs::write(ts_dir.join("package.json"), "{}").unwrap();
        fs::write(ts_dir.join("tsconfig.json"), "{}").unwrap();
        assert_eq!(detect_project_kind(&ts_dir), ProjectKind::TypeScript);

        let py_dir = dir.path().join("py-app");
        fs::create_dir_all(&py_dir).unwrap();
        fs::write(py_dir.join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_project_kind(&py_dir), ProjectKind::Python);

        let dotnet_dir = dir.path().join("net-app");
        fs::create_dir_all(&dotnet_dir).unwrap();
        fs::write(dotnet_dir.join("App.csproj"), "<Project/>").unwrap();
        assert_eq!(detect_project_kind(&dotnet_dir), ProjectKind::DotNet);
    }

    #[test]
    fn records_and_deduplicates_recent_projects() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("recent-projects.json");
        let proj_a = dir.path().join("proj-a");
        let proj_b = dir.path().join("proj-b");
        fs::create_dir_all(&proj_a).unwrap();
        fs::create_dir_all(&proj_b).unwrap();

        record_recent_project_in(&store, &proj_a, 100).unwrap();
        record_recent_project_in(&store, &proj_b, 200).unwrap();
        record_recent_project_in(&store, &proj_a, 300).unwrap();

        let loaded = load_recent_file(&store);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(
            Path::new(&loaded.entries[0].path),
            fs::canonicalize(&proj_a).unwrap()
        );
        assert_eq!(loaded.entries[0].last_opened_unix, 300);
    }

    #[test]
    fn collects_recent_then_discovered_projects_and_filters() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join("Projects");
        let alpha = projects.join("Alpha");
        let beta = projects.join("Beta");
        fs::create_dir_all(&alpha).unwrap();
        fs::create_dir_all(&beta).unwrap();
        fs::write(alpha.join("Cargo.toml"), "").unwrap();
        fs::write(beta.join("pubspec.yaml"), "").unwrap();

        let entries = collect_project_entries_with_inputs(
            &[],
            std::slice::from_ref(&beta),
            Some(home.path()),
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Beta");
        assert!(entries[0].is_recent);
        assert_eq!(entries[0].kind, ProjectKind::Flutter);
        assert_eq!(entries[1].name, "Alpha");
        assert!(!entries[1].is_recent);
        assert_eq!(entries[1].kind, ProjectKind::Rust);

        let filtered = filter_projects(&entries, "rust");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "Alpha");

        assert_eq!(
            shorten_display_path(&alpha, Some(home.path())),
            "~/Projects/Alpha"
        );
    }
}
