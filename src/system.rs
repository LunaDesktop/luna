//! Realny stan systemu dla paska: użytkownik, dźwięk (PipeWire/`wpctl`), Wi-Fi (`nmcli`),
//! Bluetooth (`bluetoothctl`), bateria (`/sys/class/power_supply`). Wartości są
//! odpytywane w tle i podawane UI.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Stan baterii.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    /// Poziom naładowania w procentach.
    pub percent: u8,
    /// Czy bateria się ładuje (albo jest pełna).
    pub charging: bool,
    /// Szacowany czas pracy na baterii (minuty); `None` gdy ładowanie/brak danych.
    pub minutes: Option<u32>,
}

/// Migawka stanu systemu (to, co pasek pokazuje naprawdę).
#[derive(Debug, Clone, Default)]
pub struct SystemStatus {
    pub user: String,
    /// `(procent, wyciszone)`.
    pub volume: Option<(u8, bool)>,
    /// Opis domyślnego wyjścia audio (`node.description` z PipeWire).
    pub audio_sink: Option<String>,
    /// `(włączone, SSID gdy połączone)`.
    pub wifi: Option<(bool, Option<String>)>,
    /// `(włączony, liczba podłączonych urządzeń)`; `None` = brak adaptera.
    pub bluetooth: Option<(bool, usize)>,
    /// Aktywny profil zasilania z `power-profiles-daemon` (gdy działa).
    pub power_profile: Option<String>,
    /// Stan baterii; `None` = brak baterii (np. komputer stacjonarny).
    pub battery: Option<BatteryStatus>,
    /// Jasność ekranu (procent), gdy jest podświetlenie (`/sys/class/backlight`).
    pub brightness: Option<u8>,
}

impl SystemStatus {
    /// Odczytuje bieżący stan (dzieła polecenia; wołać poza wątkiem UI).
    pub fn poll() -> Self {
        Self {
            user: user_name(),
            volume: read_volume(),
            audio_sink: audio_sink(),
            wifi: wifi(),
            bluetooth: bluetooth(),
            power_profile: power_profile(),
            battery: battery(),
            brightness: read_brightness(),
        }
    }
}

/// Bieżąca głośność i wyciszenie domyślnego wyjścia (`(procent, wyciszone)`).
pub fn read_volume() -> Option<(u8, bool)> {
    volume()
}

/// Bieżąca jasność ekranu w procentach (z `/sys/class/backlight`), gdy dostępna.
pub fn read_brightness() -> Option<u8> {
    let dir = backlight_dir()?;
    let cur: u32 = std::fs::read_to_string(dir.join("brightness"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let max: u32 = std::fs::read_to_string(dir.join("max_brightness"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if max == 0 {
        return None;
    }
    Some(((cur * 100 + max / 2) / max) as u8)
}

/// Ustawia jasność ekranu (procent). Bez uprawnień do sysfs cicho nic nie robi.
pub fn set_brightness(percent: u8) {
    let Some(dir) = backlight_dir() else { return };
    let Ok(text) = std::fs::read_to_string(dir.join("max_brightness")) else {
        return;
    };
    let Ok(max) = text.trim().parse::<u32>() else {
        return;
    };
    if max == 0 {
        return;
    }
    let value = (percent as u32 * max + 50) / 100;
    let _ = std::fs::write(dir.join("brightness"), format!("{value}\n"));
}

/// Katalog pierwszego podświetlenia (pierwszy wpis `/sys/class/backlight`).
fn backlight_dir() -> Option<std::path::PathBuf> {
    std::fs::read_dir("/sys/class/backlight")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .next()
}

/// Kolejny poziom jasności po kliknięciu (cykl 25 → 50 → 75 → 100 → 25).
pub fn next_brightness_level(current: u8) -> u8 {
    for level in [25u8, 50, 75, 100] {
        if current + 6 < level {
            return level;
        }
    }
    25
}

/// Współdzielony stan, aktualizowany w tle.
pub type Shared = Arc<Mutex<SystemStatus>>;

/// Startuje wątek odpytywania (od razu, potem co ~2 s) i zwraca współdzielony stan.
///
/// **Nie blokuje wołającego.** `SystemStatus::poll()` uruchamia podprocesy (`nmcli`,
/// `wpctl`), które zaraz po zalogowaniu potrafią czekać sekundy (NetworkManager/PipeWire
/// jeszcze wstają). Wcześniej wołaliśmy je w wątku UI, przez co panel pojawiał się z
/// opóźnieniem — teraz startujemy z samą nazwą użytkownika (odczyt z otoczenia) i
/// dopełniamy stan już z wątku.
///
pub fn spawn() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(SystemStatus {
        user: user_name(),
        ..SystemStatus::default()
    }));
    let writer = Arc::clone(&shared);
    std::thread::spawn(move || loop {
        let fresh = SystemStatus::poll();
        if let Ok(mut s) = writer.lock() {
            *s = fresh;
        }
        std::thread::sleep(Duration::from_secs(2));
    });
    shared
}

/// Lżejszy wariant [`spawn`] dla Ustawień: bez Wi‑Fi i Bluetooth.
///
/// Ustawienia potrzebują tylko głośności, jasności, baterii i profilu zasilania;
/// odpytywanie `nmcli`/`bluetoothctl` co 2 s byłoby zbędnym obciążeniem.
pub fn spawn_light() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(SystemStatus {
        user: user_name(),
        ..SystemStatus::default()
    }));
    let writer = Arc::clone(&shared);
    std::thread::spawn(move || loop {
        let fresh = SystemStatus {
            user: user_name(),
            volume: read_volume(),
            audio_sink: audio_sink(),
            power_profile: power_profile(),
            battery: battery(),
            brightness: read_brightness(),
            ..SystemStatus::default()
        };
        if let Ok(mut s) = writer.lock() {
            *s = fresh;
        }
        std::thread::sleep(Duration::from_secs(2));
    });
    shared
}

/// Uruchamia wątek śledzący **systemowy motyw** (portal `org.freedesktop.appearance`).
///
/// Wynik zapisujemy w `$XDG_CONFIG_HOME/luna/system-dark`, skąd czytają go wszystkie
/// procesy Luny (platforma aplikacji i kompozytor). Dzięki temu tryb `Auto` działa
/// bez przekazywania zmiennych środowiskowych do już uruchomionych programów.
pub fn watch_system_theme() {
    std::thread::spawn(|| loop {
        sync_system_dark();
        std::thread::sleep(Duration::from_secs(15));
    });
}

fn sync_system_dark() {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return;
    };
    let Ok(reply) = conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Settings"),
        "Read",
        &("org.freedesktop.appearance", "color-scheme"),
    ) else {
        return;
    };
    let Ok(value) = reply.body().deserialize::<zbus::zvariant::OwnedValue>() else {
        return;
    };
    // Portal: 1 = preferuj ciemny, 2 = preferuj jasny, 0 = brak preferencji.
    let Ok(scheme) = u32::try_from(value) else {
        return;
    };
    let dark = match scheme {
        1 => true,
        2 => false,
        _ => return,
    };
    let _ = libluna_core::appearance::set_system_dark(dark);
}

/// Włącza/wyłącza Wi-Fi (przełącznik „Wi-Fi” w pasku).
/// Włącza/wyłącza radio Wi-Fi (NetworkManager przez D-Bus).
pub fn set_wifi(on: bool) {
    let _ = crate::network::set_wifi_enabled(on);
}

/// Czy w systemie jest adapter Bluetooth (`/sys/class/bluetooth`).
pub fn bluetooth_available() -> bool {
    std::fs::read_dir("/sys/class/bluetooth")
        .ok()
        .map(|mut d| d.any(|e| e.is_ok()))
        .unwrap_or(false)
}

/// Włącza/wyłącza adapter Bluetooth.
pub fn set_bluetooth(on: bool) {
    if bluetooth_available() {
        spawn_cmd("bluetoothctl", &["power", if on { "on" } else { "off" }]);
    }
}

/// Włącza/wyłącza tryb samolotowy (wszystkie radia).
pub fn set_airplane(on: bool) {
    set_wifi(!on);
    set_bluetooth(!on);
}

/// Czy tryb „nie przeszkadzać” jest włączony (plik `$XDG_CONFIG_HOME/luna/do-not-disturb`).
pub fn dnd() -> bool {
    state_path("do-not-disturb")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Zapisuje stan trybu „nie przeszkadzać”.
pub fn set_dnd(on: bool) {
    let Some(path) = state_path("do-not-disturb") else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if on { "1" } else { "0" });
}

/// Ścieżka pliku stanu w katalogu konfiguracji Luny.
fn state_path(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("luna").join(name))
}

/// Siła night lightu (0–100) zapisana przez powłokę.
pub fn night_light_percent() -> u8 {
    state_path("night-light")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u8>().ok())
        .map(|v| v.min(100))
        .unwrap_or(0)
}

/// Zapisuje siłę night lightu (stosuje ją kompozytor przez gniazdo sterowania).
pub fn set_night_light_percent(percent: u8) {
    write_state("night-light", &percent.min(100).to_string());
}

/// Czy tryb oszczędzania jest zapisany jako włączony.
pub fn power_saver() -> bool {
    state_path("power-saver")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Zapisuje stan trybu oszczędzania.
pub fn set_power_saver(on: bool) {
    write_state("power-saver", if on { "1" } else { "0" });
}

fn write_state(name: &str, value: &str) {
    let Some(path) = state_path(name) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, value);
}

/// Czy `power-profiles-daemon` jest dostępny (`powerprofilesctl` w `PATH`).
pub fn ppd_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path).any(|dir| dir.join("powerprofilesctl").is_file())
            })
            .unwrap_or(false)
    })
}

/// Bieżący profil zasilania (np. `balanced`, `power-saver`) albo `None`.
fn power_profile() -> Option<String> {
    if !ppd_available() {
        return None;
    }
    run("powerprofilesctl", &["get"])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Ustawia profil zasilania w `power-profiles-daemon`.
pub fn set_power_profile(profile: &str) {
    spawn_cmd("powerprofilesctl", &["set", profile]);
}

/// Przełącza wyciszenie domyślnego wyjścia audio.
pub fn toggle_mute() {
    spawn_cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
}

/// Ustawia głośność domyślnego wyjścia (w procentach 0..100).
pub fn set_volume(percent: u8) {
    let p = (percent as f64) / 100.0;
    spawn_cmd(
        "wpctl",
        &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{p:.2}")],
    );
}

/// Zmienia głośność o `delta` procent (np. +5 lub -5).
pub fn adjust_volume(current: u8, delta: i8) -> u8 {
    let next = (current as i16 + delta as i16).clamp(0, 100) as u8;
    set_volume(next);
    next
}

/// Czy synchronizacja czasu (NTP) jest włączona w systemie.
pub fn ntp_enabled() -> Option<bool> {
    let output = Command::new("timedatectl")
        .env("LC_ALL", "C")
        .args(["show", "-p", "NTP", "--value"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn spawn_cmd(program: &str, args: &[&str]) {
    let _ = Command::new(program).args(args).spawn();
}

/// Uruchamia polecenie z limitem czasu (dla usług, które potrafią nie odpowiedzieć).
fn run_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .env("LC_ALL", "C")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                let out = child.wait_with_output().ok()?;
                if status.success() {
                    return Some(String::from_utf8_lossy(&out.stdout).into_owned());
                }
                return None;
            }
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_owned())
}

/// Uruchamia polecenie (bez locale, by pola `nmcli` były angielskie) i zwraca stdout.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

fn volume() -> Option<(u8, bool)> {
    // `wpctl get-volume` → „Volume: 0.40” lub „Volume: 0.40 [MUTED]”.
    parse_volume(&run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?)
}

/// Parsuje wynik `wpctl get-volume` → `(procent, wyciszone)`.
fn parse_volume(out: &str) -> Option<(u8, bool)> {
    let muted = out.contains("MUTED");
    let value: f64 = out.split_whitespace().nth(1)?.parse().ok()?;
    Some(((value * 100.0).round() as u8, muted))
}

fn wifi() -> Option<(bool, Option<String>)> {
    // Jedno źródło prawdy: ta sama warstwa D‑Bus, z której korzystają Ustawienia.
    crate::network::wifi_state()
}

fn battery() -> Option<BatteryStatus> {
    for entry in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("BAT") {
            continue;
        }
        let dir = entry.path();
        let cap: u8 = std::fs::read_to_string(dir.join("capacity"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
        let status = status.trim();
        let charging = matches!(status, "Charging" | "Full");
        let minutes = if status == "Discharging" {
            remaining_minutes(&dir)
        } else {
            None
        };
        return Some(BatteryStatus {
            percent: cap,
            charging,
            minutes,
        });
    }
    None
}

/// Szacuje pozostały czas pracy na baterii (minuty) z `energy_*` lub `charge_*`.
fn remaining_minutes(dir: &Path) -> Option<u32> {
    let read = |name: &str| -> Option<u64> {
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .parse()
            .ok()
    };
    if let (Some(now), Some(rate)) = (read("energy_now"), read("power_now")) {
        if rate > 0 {
            return Some((now.saturating_mul(60) / rate) as u32);
        }
    }
    if let (Some(now), Some(rate)) = (read("charge_now"), read("current_now")) {
        if rate > 0 {
            return Some((now.saturating_mul(60) / rate) as u32);
        }
    }
    None
}

/// Odczytuje adapter Bluetooth: `(włączony, liczba podłączonych urządzeń)`.
fn bluetooth() -> Option<(bool, usize)> {
    if !bluetooth_available() {
        return None;
    }
    let timeout = Duration::from_millis(1200);
    let show = run_timeout("bluetoothctl", &["show"], timeout)?;
    let powered = show.lines().any(|l| l.trim() == "Powered: yes");
    let connected = run_timeout("bluetoothctl", &["devices", "Connected"], timeout)
        .map(|o| {
            o.lines()
                .filter(|l| l.trim_start().starts_with("Device "))
                .count()
        })
        .unwrap_or(0);
    Some((powered, connected))
}

/// Opis domyślnego wyjścia audio (`node.description` z `wpctl inspect`).
fn audio_sink() -> Option<String> {
    let out = run("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"])?;
    parse_audio_sink(&out)
}

fn parse_audio_sink(out: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let line = line.trim_start_matches(['*', ' ']);
        let value = line.strip_prefix("node.description =")?.trim();
        let value = value.trim_matches('"');
        (!value.is_empty()).then(|| value.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_cycles_levels() {
        assert_eq!(next_brightness_level(0), 25);
        assert_eq!(next_brightness_level(25), 50);
        assert_eq!(next_brightness_level(50), 75);
        assert_eq!(next_brightness_level(75), 100);
        assert_eq!(next_brightness_level(100), 25);
    }

    #[test]
    fn parses_volume_and_mute() {
        assert_eq!(parse_volume("Volume: 0.40\n"), Some((40, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((40, true)));
        assert_eq!(parse_volume("Volume: 1.00\n"), Some((100, false)));
        assert_eq!(parse_volume("Nieoczekiwane\n"), None);
    }

    #[test]
    fn parses_audio_sink_description() {
        let out = "id 45, type PipeWire:Interface:Node\n  * node.description = \"Głośniki wbudowane\"\n  * node.name = \"alsa_output\"\n";
        assert_eq!(parse_audio_sink(out).as_deref(), Some("Głośniki wbudowane"));
        assert_eq!(parse_audio_sink("brak\n"), None);
    }
}
