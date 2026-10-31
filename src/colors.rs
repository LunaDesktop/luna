//! Backend colord — profile ICC i przypisanie do urządzeń.
//!
//! Odczyt idzie przez D-Bus `org.freedesktop.ColorManager`, import i przypisanie
//! przez `pkexec colormgr` (colord nie ma prostej metody D-Bus do importu pliku).

use std::collections::HashMap;
use std::process::{Command, Stdio};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const COLORD_DEST: &str = "org.freedesktop.ColorManager";
const COLORD_PATH: &str = "/org/freedesktop/ColorManager";
const MANAGER_IFACE: &str = "org.freedesktop.ColorManager";
const DEVICE_IFACE: &str = "org.freedesktop.ColorManager.Device";
const PROFILE_IFACE: &str = "org.freedesktop.ColorManager.Profile";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Profil koloru (ICC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorProfile {
    pub id: String,
    pub title: String,
    pub filename: Option<String>,
    /// Rodzaj profilu (np. `display-device`, `printer`).
    pub kind: Option<String>,
}

/// Urządzenie zarządzane przez colord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorDevice {
    pub id: String,
    pub model: String,
    /// `display`, `printer`, …
    pub kind: String,
    /// Identyfikatory profili przypisanych do urządzenia.
    pub profiles: Vec<String>,
    pub enabled: bool,
}

/// Migawka colord.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub available: bool,
    pub devices: Vec<ColorDevice>,
    pub profiles: Vec<ColorProfile>,
    pub error: Option<String>,
}

fn system() -> Result<Connection, String> {
    Connection::system().map_err(|err| format!("D-Bus (system): {err}"))
}

fn all_props(conn: &Connection, path: &str, iface: &str) -> HashMap<String, OwnedValue> {
    conn.call_method(Some(COLORD_DEST), path, Some(PROPS), "GetAll", &iface)
        .ok()
        .and_then(|reply| {
            reply
                .body()
                .deserialize::<HashMap<String, OwnedValue>>()
                .ok()
        })
        .unwrap_or_default()
}

fn as_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn as_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok())
}

/// Wartość `Profiles` (`ao` ścieżek obiektów; toleruje też `as` identyfikatorów).
fn as_profile_paths(props: &HashMap<String, OwnedValue>) -> Vec<String> {
    let Some(value) = props.get("Profiles") else {
        return Vec::new();
    };
    if let Ok(paths) = Vec::<OwnedObjectPath>::try_from(value.clone()) {
        return paths.into_iter().map(|path| path.to_string()).collect();
    }
    Vec::<String>::try_from(value.clone()).unwrap_or_default()
}

/// `Manager.GetDevices`/`GetProfiles` → lista ścieżek obiektów.
fn get_paths(conn: &Connection, method: &str) -> Result<Vec<OwnedObjectPath>, String> {
    conn.call_method(
        Some(COLORD_DEST),
        COLORD_PATH,
        Some(MANAGER_IFACE),
        method,
        &(),
    )
    .map_err(|err| err.to_string())
    .and_then(|reply| {
        reply
            .body()
            .deserialize::<Vec<OwnedObjectPath>>()
            .map_err(|err| err.to_string())
    })
}

/// Migawka colord; brak usługi to `available=false` bez błędu.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    let conn = match system() {
        Ok(conn) => conn,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    // colord nie odpowiada → brak usługi, nie błąd.
    let Ok(profile_paths) = get_paths(&conn, "GetProfiles") else {
        return snap;
    };
    let Ok(device_paths) = get_paths(&conn, "GetDevices") else {
        return snap;
    };
    snap.available = true;

    let mut profiles = Vec::new();
    let mut ids_by_path: HashMap<String, String> = HashMap::new();
    for path in profile_paths {
        let path = path.to_string();
        let props = all_props(&conn, &path, PROFILE_IFACE);
        let id = as_string(&props, "ProfileId").unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        ids_by_path.insert(path.clone(), id.clone());
        profiles.push(ColorProfile {
            title: as_string(&props, "Title").unwrap_or_default(),
            filename: as_string(&props, "Filename").filter(|name| !name.is_empty()),
            kind: as_string(&props, "Kind"),
            id,
        });
    }
    sort_profiles(&mut profiles);

    let mut devices = Vec::new();
    for path in device_paths {
        let path = path.to_string();
        let props = all_props(&conn, &path, DEVICE_IFACE);
        let id = as_string(&props, "DeviceId").unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let profiles = as_profile_paths(&props)
            .into_iter()
            .map(|profile| ids_by_path.get(&profile).cloned().unwrap_or(profile))
            .collect();
        devices.push(ColorDevice {
            model: as_string(&props, "Model").unwrap_or_default(),
            kind: as_string(&props, "Kind").unwrap_or_default(),
            enabled: as_bool(&props, "Enabled").unwrap_or(false),
            profiles,
            id,
        });
    }
    sort_devices(&mut devices);

    snap.profiles = profiles;
    snap.devices = devices;
    snap
}

/// Profile alfabetycznie po tytule, potem po id.
fn sort_profiles(profiles: &mut [ColorProfile]) {
    profiles.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Urządzenia alfabetycznie po modelu, potem po id.
fn sort_devices(devices: &mut [ColorDevice]) {
    devices.sort_by(|a, b| {
        a.model
            .to_lowercase()
            .cmp(&b.model.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Uruchamia program: stdout jako wynik, ostatnia niepusta linia stderr jako błąd.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("{program}: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
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

fn validate_id(id: &str, what: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("Pusty identyfikator {what}"));
    }
    if id.starts_with('-') || id.chars().any(char::is_whitespace) {
        return Err(format!("Nieprawidłowy identyfikator {what}"));
    }
    Ok(())
}

/// Importuje profil ICC (`pkexec colormgr import-profile`).
pub fn import(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Pusta ścieżka pliku".to_owned());
    }
    if path.starts_with('-') {
        return Err("Ścieżka nie może zaczynać się od -".to_owned());
    }
    run("pkexec", &["colormgr", "import-profile", path]).map(|_| ())
}

/// Przypisuje profil do urządzenia (`pkexec colormgr device-add-profile`).
pub fn assign(device_id: &str, profile_id: &str) -> Result<(), String> {
    validate_id(device_id, "urządzenia")?;
    validate_id(profile_id, "profilu")?;
    run(
        "pkexec",
        &["colormgr", "device-add-profile", device_id, profile_id],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, title: &str) -> ColorProfile {
        ColorProfile {
            id: id.to_owned(),
            title: title.to_owned(),
            filename: None,
            kind: None,
        }
    }

    fn device(id: &str, model: &str) -> ColorDevice {
        ColorDevice {
            id: id.to_owned(),
            model: model.to_owned(),
            kind: "display".to_owned(),
            profiles: vec![],
            enabled: true,
        }
    }

    #[test]
    fn sorts_profiles_by_title() {
        let mut profiles = vec![
            profile("icc-b", "sRGB"),
            profile("icc-a", "Adobe RGB"),
            profile("icc-c", "sRGB"),
        ];
        sort_profiles(&mut profiles);
        let ids: Vec<_> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["icc-a", "icc-b", "icc-c"]);
    }

    #[test]
    fn sorts_devices_by_model() {
        let mut devices = vec![
            device("xrandr-1", "Dell U2720Q"),
            device("xrandr-0", "Acer X27"),
        ];
        sort_devices(&mut devices);
        let ids: Vec<_> = devices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["xrandr-0", "xrandr-1"]);
    }

    #[test]
    fn validates_ids() {
        assert!(validate_id("icc-abc", "profilu").is_ok());
        for bad in ["", "-icc", "icc abc"] {
            assert!(validate_id(bad, "profilu").is_err(), "{bad}");
        }
    }
}
