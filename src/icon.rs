use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};
use waybar_cffi::gtk::{
    gio::{AppInfo, DesktopAppInfo, FileIcon},
    prelude::{AppInfoExt, Cast, FileExt, IconExt},
};

/// A cache for taskbar icons.
#[derive(Debug, Clone, Default)]
pub struct Cache(Arc<Mutex<HashMap<String, PathBuf>>>);

impl Cache {
    /// Look up an icon for the given application ID.
    #[tracing::instrument(level = "TRACE", ret)]
    pub fn lookup(&self, id: &str) -> Option<PathBuf> {
        let mut cache = self.0.lock().expect("icon cache lock");
        if !cache.contains_key(id) {
            if let Some(path) = lookup(id) {
                cache.insert(id.to_string(), path);
            }
        }
        cache.get(id).cloned()
    }
}

fn lookup(id: &str) -> Option<PathBuf> {
    if let Some(icon) = lookup_icon(id) {
        return Some(icon);
    }

    if let Some(icon) = lookup_by_startup_wm_class(id) {
        return Some(icon);
    }

    // Steam games report themselves as e.g. "steam_app_123456" but store their icons as
    // "steam_icon_123456" in hicolor — simple prefix swap.
    if let Some(steam_id) = id.strip_prefix("steam_app_") {
        let icon_name = format!("steam_icon_{steam_id}");
        if let Some(path) = lookup_icon(&icon_name).or_else(|| lookup_icon_hicolor(&icon_name)) {
            return Some(path);
        }
    }

    // Wine apps report their process name as the app_id (e.g. "notepad++.exe"). There's no
    // consistent icon naming scheme so we have to go hunting through .desktop files.
    if id.ends_with(".exe") {
        if let Some(path) = lookup_wine_exe(id) {
            return Some(path);
        }
    }

    // KDE applications are special, so we'll go hunt for them ourselves. Again, this is loosely
    // adapted from wlr/taskbar.
    for dir in XDG_DATA_DIRS.iter() {
        for prefix in [
            "applications/",
            "applications/kde/",
            "applications/org.kde.",
        ] {
            for suffix in ["", ".desktop"] {
                let path = dir.join(format!("{prefix}{id}{suffix}"));
                if let Some(info) = DesktopAppInfo::from_filename(&path) {
                    if let Some(path) = info.icon_path() {
                        return Some(path);
                    }
                }
            }
        }
    }

    // This is _very_ roughly adapted from the wlr/taskbar module built into Waybar.
    let infos = DesktopAppInfo::search(id);
    for possible in infos.into_iter().flatten() {
        if let Some(info) = DesktopAppInfo::new(&possible) {
            if let Some(path) = info.icon_path() {
                return Some(path);
            }
        }
    }

    None
}

/// Wine reports its windows as e.g. "notepad++.exe" — not exactly a freedesktop icon name.
/// We strip the .exe and scan wine's .desktop files to find a match.
fn lookup_wine_exe(exe_name: &str) -> Option<PathBuf> {
    let stem = exe_name.trim_end_matches(".exe").to_lowercase();

    for dir in XDG_DATA_DIRS.iter() {
        let wine_dir = dir.join("applications/wine");
        let Ok(entries) = std::fs::read_dir(&wine_dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Wine likes to nest .desktop files in subdirectories like
            // applications/wine/Programs/Notepad++/ — so we go one level deeper.
            if path.is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub_entry in sub_entries.flatten() {
                        let sub_path = sub_entry.path();
                        if sub_path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                            if let Some(icon) = try_match_wine_desktop(&sub_path, &stem) {
                                return Some(icon);
                            }
                        }
                    }
                }
                continue;
            }

            if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                if let Some(icon) = try_match_wine_desktop(&path, &stem) {
                    return Some(icon);
                }
            }
        }
    }

    None
}

/// Try to match a single .desktop file against an exe stem (e.g. "notepad++").
/// We check Exec= first since it usually contains the exe path, then fall back to Name=.
fn try_match_wine_desktop(path: &std::path::Path, exe_stem: &str) -> Option<PathBuf> {
    let info = DesktopAppInfo::from_filename(path)?;

    // Exec= usually looks like "env WINEPREFIX=... wine .../notepad++.exe"
    if let Some(exec) = info.commandline() {
        if exec.to_string_lossy().to_lowercase().contains(exe_stem) {
            return info.icon_path();
        }
    }

    // Fall back to Name= — less precise but catches wrapper scripts and renamed launchers.
    let name = info.name().to_string().to_lowercase();
    if name.contains(exe_stem) {
        return info.icon_path();
    }

    None
}

fn lookup_by_startup_wm_class(wm_class: &str) -> Option<PathBuf> {
    for info in AppInfo::all() {
        let Ok(desktop_info) = info.dynamic_cast::<DesktopAppInfo>() else {
            continue;
        };

        if desktop_info.startup_wm_class().as_deref() == Some(wm_class) {
            if let Some(icon) = desktop_info.icon() {
                if let Ok(file_icon) = icon.downcast::<FileIcon>() {
                    return file_icon.file().path();
                }
            }
        }
    }
    None
}

fn lookup_icon(id: &str) -> Option<PathBuf> {
    if let Some(path) = freedesktop_icons::lookup(id).with_size(512).find() {
        return Some(path);
    }
    if let Some(path) = linicon::lookup_icon(id)
        .with_size(512)
        .filter_map(|result| result.ok())
        .next()
    {
        return Some(path.path);
    }
    None
}

/// Bypass the theme engine and look directly in hicolor — useful for Wine/Steam icons that
/// exist on disk but aren't registered with any theme.
fn lookup_icon_hicolor(id: &str) -> Option<PathBuf> {
    for dir in XDG_DATA_DIRS.iter() {
        for size in ["256x256", "128x128", "64x64", "48x48", "32x32", "16x16"] {
            for ext in ["png", "svg", "xpm"] {
                let path = dir.join(format!("icons/hicolor/{size}/apps/{id}.{ext}"));
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }
    None
}

static XDG_DATA_DIRS: LazyLock<Vec<PathBuf>> = LazyLock::new(|| {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }
    if let Ok(env) = std::env::var("XDG_DATA_DIRS") {
        dirs.extend(env.split(':').map(PathBuf::from))
    } else {
        dirs.extend(
            ["/usr/share", "/usr/local/share"]
                .into_iter()
                .map(PathBuf::from),
        );
    }
    dirs
});

trait DesktopAppInfoExt {
    fn icon_path(&self) -> Option<PathBuf>;
}

impl DesktopAppInfoExt for DesktopAppInfo {
    fn icon_path(&self) -> Option<PathBuf> {
        self.icon()
            .and_then(|icon| IconExt::to_string(&icon))
            // Also try hicolor directly in case the icon isn't registered with the theme engine.
            .and_then(|name| lookup_icon(&name).or_else(|| lookup_icon_hicolor(&name)))
    }
}
