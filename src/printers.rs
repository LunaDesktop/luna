//! Backend CUPS — drukarki przez `lpstat` i `lpadmin`.
//!
//! Warstwa jest cienka: stan czyta standardowy `lpstat`, a zmiany wykonuje
//! `pkexec lpadmin`/`cupsenable`/`cupsdisable`. Nazwy i URI walidujemy przed
//! wywołaniem, żeby nie dało się ich pomylić z opcjami poleceń.

use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Najdłuższa nazwa drukarki akceptowana przez CUPS.
pub const MAX_NAME: usize = 127;

/// Pojedyncza drukarka.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    /// Tekst stanu z `lpstat -p` (np. `is idle.` albo `disabled since …`).
    pub info: String,
    pub is_default: bool,
    /// Czy drukarka przyjmuje zadania (nie jest `disabled`).
    pub enabled: bool,
    /// URI urządzenia z `lpstat -v` (np. `socket://…`, `ipp://…`).
    pub uri: Option<String>,
}

/// Migawka drukarek dla panelu/Ustawień.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub available: bool,
    pub printers: Vec<Printer>,
    pub default: Option<String>,
    pub error: Option<String>,
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

/// Migawka drukarek: `lpstat -p -d` + `lpstat -v`.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    let status = match run("lpstat", &["-p", "-d"]) {
        Ok(out) => out,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    snap.available = true;
    let uris = run("lpstat", &["-v"])
        .map(|out| parse_uris(&out))
        .unwrap_or_default();
    let (printers, default) = printers_from(&status, &uris);
    snap.printers = printers;
    snap.default = default;
    snap
}

/// Składa drukarki z wyjścia `lpstat` (czysta logika, testowalna bez CUPS).
fn printers_from(status: &str, uris: &HashMap<String, String>) -> (Vec<Printer>, Option<String>) {
    let default = parse_default(status);
    let mut printers: Vec<Printer> = parse_status(status)
        .into_iter()
        .map(|(name, enabled, info)| {
            let is_default = default.as_deref() == Some(name.as_str());
            Printer {
                uri: uris.get(&name).cloned(),
                is_default,
                enabled,
                info,
                name,
            }
        })
        .collect();
    sort_printers(&mut printers);
    (printers, default)
}

/// Parsuje `lpstat -p` → `(nazwa, enabled, tekst stanu)`.
fn parse_status(output: &str) -> Vec<(String, bool, String)> {
    let mut printers = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("printer ") else {
            continue;
        };
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() {
            continue;
        }
        let info = rest[end..].trim().to_owned();
        let enabled = !rest.contains("disabled");
        printers.push((name.to_owned(), enabled, info));
    }
    printers
}

/// Parsuje „system default destination: <nazwa>”.
fn parse_default(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.strip_prefix("system default destination:")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    })
}

/// Parsuje `lpstat -v` → mapa nazwa → URI.
fn parse_uris(output: &str) -> HashMap<String, String> {
    let mut uris = HashMap::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("device for ") else {
            continue;
        };
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon].trim();
        let uri = rest[colon + 1..].trim();
        if !name.is_empty() && !uri.is_empty() {
            uris.insert(name.to_owned(), uri.to_owned());
        }
    }
    uris
}

/// Domyślna drukarka najpierw, reszta alfabetycznie (bez wielkości liter).
fn sort_printers(printers: &mut [Printer]) {
    printers.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Nazwa drukarki: `[A-Za-z0-9_.-]`, 1..=127 znaków.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err("Nazwa drukarki musi mieć od 1 do 127 znaków".to_owned());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    {
        return Err("Nazwa drukarki może zawierać tylko litery, cyfry, _, . i -".to_owned());
    }
    Ok(())
}

/// URI urządzenia: niepuste, bez białych znaków, nie zaczyna się od `-`.
pub fn validate_uri(uri: &str) -> Result<(), String> {
    if uri.is_empty() {
        return Err("URI urządzenia nie może być puste".to_owned());
    }
    if uri.starts_with('-') {
        return Err("URI nie może zaczynać się od -".to_owned());
    }
    if uri.chars().any(char::is_whitespace) {
        return Err("URI nie może zawierać białych znaków".to_owned());
    }
    Ok(())
}

/// Ustawia drukarkę domyślną (`lpadmin -d`).
pub fn set_default(name: &str) -> Result<(), String> {
    validate_name(name)?;
    run("pkexec", &["lpadmin", "-d", name]).map(|_| ())
}

/// Włącza/zatrzymuje przyjmowanie zadań (`cupsenable`/`cupsdisable`).
pub fn set_enabled(name: &str, enabled: bool) -> Result<(), String> {
    validate_name(name)?;
    let command = if enabled { "cupsenable" } else { "cupsdisable" };
    run("pkexec", &[command, name]).map(|_| ())
}

/// Usuwa drukarkę (`lpadmin -x`).
pub fn remove(name: &str) -> Result<(), String> {
    validate_name(name)?;
    run("pkexec", &["lpadmin", "-x", name]).map(|_| ())
}

/// Dodaje drukarkę (`lpadmin -p … -E -v … -m …`); `model` może być `everywhere`.
pub fn add(name: &str, uri: &str, model: &str) -> Result<(), String> {
    validate_name(name)?;
    validate_uri(uri)?;
    if model.is_empty() || model.starts_with('-') || model.chars().any(char::is_whitespace) {
        return Err("Nieprawidłowy model drukarki".to_owned());
    }
    run(
        "pkexec",
        &["lpadmin", "-p", name, "-E", "-v", uri, "-m", model],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"printer Office_HP is idle.  enabled since Thu 02 Jan 2025 12:00:00 PM
printer Old_Laser disabled since Mon 30 Dec 2024 09:00:00 AM -
printer Kitchen now printing Kitchen-123.  enabled since Thu 02 Jan 2025 12:05:00 PM
system default destination: Office_HP
"#;

    #[test]
    fn parses_status_and_default() {
        let printers = parse_status(STATUS);
        assert_eq!(printers.len(), 3);
        assert_eq!(printers[0].0, "Office_HP");
        assert!(printers[0].1);
        assert_eq!(
            printers[0].2,
            "is idle.  enabled since Thu 02 Jan 2025 12:00:00 PM"
        );
        assert_eq!(printers[1].0, "Old_Laser");
        assert!(!printers[1].1);
        assert_eq!(printers[2].0, "Kitchen");
        assert!(printers[2].1);
        assert_eq!(parse_default(STATUS).as_deref(), Some("Office_HP"));
    }

    #[test]
    fn parses_device_uris() {
        let uris = parse_uris(
            "device for Office_HP: socket://192.168.1.50\ndevice for Kitchen: ipps://kitchen.local/ipp/print\n",
        );
        assert_eq!(
            uris.get("Office_HP").map(String::as_str),
            Some("socket://192.168.1.50")
        );
        assert_eq!(
            uris.get("Kitchen").map(String::as_str),
            Some("ipps://kitchen.local/ipp/print")
        );
        assert!(!uris.contains_key("Old_Laser"));
    }

    #[test]
    fn merges_uris_and_flags_default() {
        let uris = parse_uris("device for Office_HP: socket://1.2.3.4\n");
        let (printers, default) = printers_from(STATUS, &uris);
        assert_eq!(default.as_deref(), Some("Office_HP"));
        assert_eq!(printers.len(), 3);
        // Domyślna drukarka jest pierwsza.
        assert_eq!(printers[0].name, "Office_HP");
        let office = printers.iter().find(|p| p.name == "Office_HP").unwrap();
        assert!(office.is_default);
        assert_eq!(office.uri.as_deref(), Some("socket://1.2.3.4"));
        let old = printers.iter().find(|p| p.name == "Old_Laser").unwrap();
        assert!(old.uri.is_none());
        assert!(!old.enabled);
    }

    #[test]
    fn validates_names() {
        for name in [
            "office",
            "Office_HP",
            "x.y-z",
            "-x",
            "a".repeat(127).as_str(),
        ] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        for name in ["", "a b", "a:b", "a@b", "a".repeat(128).as_str()] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn validates_uris() {
        for uri in ["socket://1.2.3.4", "ipp://host/ipp", "everywhere"] {
            assert!(validate_uri(uri).is_ok(), "{uri}");
        }
        for uri in ["", "-socket://x", "ipp://a b"] {
            assert!(validate_uri(uri).is_err(), "{uri}");
        }
    }
}
