//! Domyślne aplikacje — `xdg-mime`, wpisy `.desktop` i `mimeapps.list`.
//!
//! Nie trzymamy własnej bazy skojarzeń: czytamy katalogi `applications`
//! (`$XDG_DATA_HOME` + `$XDG_DATA_DIRS`) i pytamy `xdg-mime`, jaki program
//! obsługuje dany typ MIME. Zapis robi `xdg-mime default`, czyli dokładnie to
//! samo narzędzie, które używają inne aplikacje i portale.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Kategoria w Ustawieniach + typy MIME i kategorie `.desktop` do filtrowania.
#[derive(Debug, Clone, Copy)]
pub struct Category {
    /// Klucz tłumaczenia (angielski tekst).
    pub key: &'static str,
    /// Reprezentatywne typy MIME (pierwszy jest używany do odczytu).
    pub mimes: &'static [&'static str],
    /// Kategorie `.desktop`, które kwalifikują aplikację do tej roli.
    pub desktop_categories: &'static [&'static str],
}

pub const CATEGORIES: [Category; 8] = [
    Category {
        key: "Web browser",
        mimes: &[
            "x-scheme-handler/http",
            "x-scheme-handler/https",
            "text/html",
        ],
        desktop_categories: &["WebBrowser"],
    },
    Category {
        key: "Email",
        mimes: &["x-scheme-handler/mailto"],
        desktop_categories: &["Email", "MailClient"],
    },
    Category {
        key: "File manager",
        mimes: &["inode/directory"],
        desktop_categories: &["FileManager"],
    },
    Category {
        key: "Image viewer",
        mimes: &["image/png", "image/jpeg", "image/webp"],
        desktop_categories: &["ImageViewer", "Graphics"],
    },
    Category {
        key: "Video player",
        mimes: &["video/mp4", "video/x-matroska", "video/webm"],
        desktop_categories: &["VideoPlayer", "Player", "AudioVideo"],
    },
    Category {
        key: "Music player",
        mimes: &["audio/mpeg", "audio/flac", "audio/ogg"],
        desktop_categories: &["AudioPlayer", "Player", "AudioVideo"],
    },
    Category {
        key: "PDF viewer",
        mimes: &["application/pdf"],
        desktop_categories: &["Office", "Viewer"],
    },
    Category {
        key: "Text editor",
        mimes: &["text/plain"],
        desktop_categories: &["TextEditor", "Utility"],
    },
];

/// Aplikacja z wpisu `.desktop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    /// Identyfikator freedesktop, np. `firefox.desktop`.
    pub desktop_id: String,
    pub name: String,
    pub exec: String,
    pub categories: Vec<String>,
    pub mime_types: Vec<String>,
}

/// Stan domyślnych aplikacji.
#[derive(Debug, Clone, Default)]
pub struct Defaults {
    pub available: bool,
    /// Wszystkie aplikacje ze wpisów `.desktop` (posortowane po nazwie).
    pub apps: Vec<DesktopApp>,
    /// Bieżąca aplikacja dla każdej kategorii z [`CATEGORIES`].
    pub current: Vec<Option<String>>,
    pub error: Option<String>,
}

impl Defaults {
    /// Aplikacje pasujące do kategorii (po MIME lub kategorii `.desktop`).
    pub fn apps_for(&self, index: usize) -> Vec<&DesktopApp> {
        let Some(category) = CATEGORIES.get(index) else {
            return Vec::new();
        };
        self.apps
            .iter()
            .filter(|app| {
                category
                    .desktop_categories
                    .iter()
                    .any(|wanted| app.categories.iter().any(|have| have == wanted))
                    || category
                        .mimes
                        .iter()
                        .any(|mime| app.mime_types.iter().any(|have| have == mime))
            })
            .collect()
    }

    /// Nazwa wyświetlana dla bieżącej aplikacji (albo `None`).
    pub fn current_name(&self, index: usize) -> Option<&str> {
        let id = self.current.get(index)?.as_deref()?;
        self.apps
            .iter()
            .find(|app| app.desktop_id == id)
            .map(|app| app.name.as_str())
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|err| format!("{program}: {err}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("nieznany błąd");
    Err(format!("{program}: {detail}"))
}

/// Katalogi z wpisami `.desktop` w kolejności priorytetu.
fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    if let Some(home) = data_home {
        dirs.push(home.join("applications"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    for base in data_dirs.split(':').filter(|base| !base.is_empty()) {
        dirs.push(PathBuf::from(base).join("applications"));
    }
    dirs
}

/// Wczytuje wpisy `.desktop` (bez duplikatów — pierwszy katalog wygrywa).
fn scan_apps() -> Vec<DesktopApp> {
    let mut seen: BTreeMap<String, DesktopApp> = BTreeMap::new();
    for dir in desktop_dirs() {
        collect_desktop_files(&dir, &dir, &mut seen, 0);
    }
    let mut apps: Vec<DesktopApp> = seen.into_values().collect();
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

fn collect_desktop_files(
    base: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, DesktopApp>,
    depth: usize,
) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_desktop_files(base, &path, out, depth + 1);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
            continue;
        }
        let Some(id) = desktop_id(base, &path) else {
            continue;
        };
        if out.contains_key(&id) {
            continue;
        }
        if let Some(app) = parse_desktop_file(&path, &id) {
            out.insert(id, app);
        }
    }
}

/// `applications/foo/bar.desktop` → `foo-bar.desktop` (konwencja freedesktop).
fn desktop_id(base: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(base).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let text = component.as_os_str().to_str()?;
        parts.push(text.to_owned());
    }
    Some(parts.join("-"))
}

/// Parsuje wpis `.desktop`; `None` dla niepoprawnych/ukrytych/nie-aplikacji.
pub fn parse_desktop_file(path: &Path, id: &str) -> Option<DesktopApp> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_desktop_entry(&text, id)
}

/// Czysty parser wpisu (testowalny bez systemu plików).
pub fn parse_desktop_entry(text: &str, id: &str) -> Option<DesktopApp> {
    let mut in_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut kind = None;
    let mut hidden = false;
    let mut categories = Vec::new();
    let mut mime_types = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => name = Some(value.to_owned()),
            "Exec" => exec = Some(value.to_owned()),
            "Type" => kind = Some(value.to_owned()),
            "NoDisplay" | "Hidden" => hidden = hidden || matches!(value, "true" | "True" | "1"),
            "Categories" => {
                categories = value
                    .split(';')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            "MimeType" => {
                mime_types = value
                    .split(';')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            _ => {}
        }
    }
    if kind.as_deref() != Some("Application") || hidden {
        return None;
    }
    let name = name.filter(|name| !name.is_empty())?;
    let exec = exec.filter(|exec| !exec.is_empty())?;
    Some(DesktopApp {
        desktop_id: id.to_owned(),
        name,
        exec,
        categories,
        mime_types,
    })
}

/// Bieżąca aplikacja dla typu MIME (`xdg-mime query default`).
fn current_for(category: &Category) -> Option<String> {
    for mime in category.mimes {
        if let Ok(value) = run("xdg-mime", &["query", "default", mime]) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Migawka: aplikacje + bieżące skojarzenia.
pub fn snapshot() -> Defaults {
    let mut defaults = Defaults {
        current: vec![None; CATEGORIES.len()],
        ..Defaults::default()
    };
    match run("xdg-mime", &["query", "default", "text/html"]) {
        Ok(_) => defaults.available = true,
        Err(err) => {
            defaults.error = Some(format!(
                "{}: {err}",
                libluna_i18n::tr("xdg-mime is not available")
            ));
            return defaults;
        }
    }
    defaults.apps = scan_apps();
    for (index, category) in CATEGORIES.iter().enumerate() {
        defaults.current[index] = current_for(category);
    }
    defaults
}

/// Walidacja identyfikatora `.desktop` (bez spacji i opcji wiersza poleceń).
pub fn validate_desktop_id(id: &str) -> Result<(), String> {
    if !id.ends_with(".desktop") || id.len() > 255 {
        return Err("niepoprawny identyfikator .desktop".to_owned());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+'))
    {
        return Err("identyfikator .desktop zawiera niedozwolone znaki".to_owned());
    }
    Ok(())
}

/// Ustawia domyślną aplikację dla kategorii (`xdg-mime default`).
pub fn set_default(category_index: usize, desktop_id: &str) -> Result<(), String> {
    let category = CATEGORIES
        .get(category_index)
        .ok_or_else(|| "nieznana kategoria".to_owned())?;
    validate_desktop_id(desktop_id)?;
    let mut args = vec!["default".to_owned(), desktop_id.to_owned()];
    args.extend(category.mimes.iter().map(|mime| (*mime).to_owned()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run("xdg-mime", &refs).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "\
[Desktop Entry]
Type=Application
Name=Firefox
Exec=firefox %u
Categories=Network;WebBrowser;
MimeType=text/html;x-scheme-handler/http;x-scheme-handler/https;
";

    #[test]
    fn parses_desktop_entry_and_skips_hidden() {
        let app = parse_desktop_entry(FIREFOX, "firefox.desktop").expect("wpis");
        assert_eq!(app.name, "Firefox");
        assert_eq!(app.exec, "firefox %u");
        assert!(app.categories.contains(&"WebBrowser".to_owned()));
        assert!(app
            .mime_types
            .contains(&"x-scheme-handler/https".to_owned()));

        let hidden = FIREFOX.replace("Type=Application", "Type=Application\nNoDisplay=true");
        assert!(parse_desktop_entry(&hidden, "ukryta.desktop").is_none());
        let library = FIREFOX.replace("Type=Application", "Type=Link");
        assert!(parse_desktop_entry(&library, "link.desktop").is_none());
    }

    #[test]
    fn desktop_id_uses_dashes_for_subdirectories() {
        let base = Path::new("/usr/share/applications");
        let path = Path::new("/usr/share/applications/kde/foo.desktop");
        assert_eq!(desktop_id(base, path).as_deref(), Some("kde-foo.desktop"));
    }

    #[test]
    fn apps_for_matches_category_or_mime() {
        let mut defaults = Defaults::default();
        defaults.apps = vec![
            parse_desktop_entry(FIREFOX, "firefox.desktop").unwrap(),
            parse_desktop_entry(
                "[Desktop Entry]\nType=Application\nName=GIMP\nExec=gimp\nCategories=Graphics;\nMimeType=image/png;\n",
                "gimp.desktop",
            )
            .unwrap(),
        ];
        let web = defaults.apps_for(0);
        assert_eq!(web.len(), 1);
        assert_eq!(web[0].name, "Firefox");
        let images = defaults.apps_for(3);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].name, "GIMP");
        assert!(defaults.apps_for(7).is_empty());
    }

    #[test]
    fn desktop_id_validation_rejects_arguments() {
        assert!(validate_desktop_id("firefox.desktop").is_ok());
        assert!(validate_desktop_id("org.kde.foo.desktop").is_ok());
        assert!(validate_desktop_id("firefox").is_err());
        assert!(validate_desktop_id("firefox.desktop --version").is_err());
        assert!(validate_desktop_id("../evil.desktop").is_err());
    }
}
