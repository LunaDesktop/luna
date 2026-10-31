//! Warstwa sieci Luny — backend NetworkManager + typy dla UI.
//!
//! `Snapshot` jest tanią migawką dla panelu i Ustawień. Cięższe odczyty D-Bus
//! (skan Wi-Fi, lista punktów dostępowych) wykonuje [`Monitor`] w wątku tła,
//! więc klatka UI nigdy nie czeka na magistralę.

mod nm;
mod types;

pub use nm::{
    connect_ethernet, connect_wifi, disconnect_device, forget_connection, forget_wifi,
    request_wifi_scan, set_autoconnect, set_wifi_enabled, set_wired_ipv4, snapshot, wifi_state,
};
pub use types::{
    split_cidr, Connectivity, Device, DeviceKind, DeviceState, Ipv4Method, KnownWifi, NetworkError,
    Security, Snapshot, WifiNetwork, WiredConfig,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Wątek odświeżający migawkę sieci bez blokowania UI.
pub struct Monitor {
    shared: Arc<Mutex<Snapshot>>,
    refresh_requested: Arc<AtomicBool>,
    scan_requested: Arc<AtomicBool>,
}

impl Monitor {
    pub fn spawn() -> Self {
        let shared: Arc<Mutex<Snapshot>> = Arc::new(Mutex::new(Snapshot::default()));
        let refresh_requested = Arc::new(AtomicBool::new(true));
        let scan_requested = Arc::new(AtomicBool::new(true));
        let (worker_shared, worker_refresh, worker_scan) = (
            Arc::clone(&shared),
            Arc::clone(&refresh_requested),
            Arc::clone(&scan_requested),
        );
        std::thread::Builder::new()
            .name("luna-network".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(200));
                let scan = worker_scan.swap(false, Ordering::Relaxed);
                let refresh = worker_refresh.swap(false, Ordering::Relaxed);
                if !scan && !refresh {
                    continue;
                }
                if scan {
                    let _ = nm::request_wifi_scan();
                    // Daj NetworkManagerowi chwilę na uzupełnienie listy AP.
                    std::thread::sleep(Duration::from_millis(350));
                }
                let snapshot = nm::snapshot();
                if let Ok(mut guard) = worker_shared.lock() {
                    *guard = snapshot;
                }
            })
            .ok();
        Self {
            shared,
            refresh_requested,
            scan_requested,
        }
    }

    /// Odświeża stan urządzeń i aktywnych połączeń (bez wymuszania skanu).
    pub fn refresh(&self) {
        self.refresh_requested.store(true, Ordering::Relaxed);
    }

    /// Wymusza skan Wi-Fi i odświeża migawkę.
    pub fn scan(&self) {
        self.scan_requested.store(true, Ordering::Relaxed);
        self.refresh_requested.store(true, Ordering::Relaxed);
    }

    /// Ostatnia migawka (kopia; tanio — to tylko małe wektory).
    pub fn current(&self) -> Snapshot {
        self.shared
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

/// Skan Wi-Fi dla paska (ten sam `Monitor`, co w Ustawieniach).
///
/// Panel nie ma własnej ścieżki `nmcli`: lista sieci, stan radia i aktywne SSID
/// pochodzą z warstwy D‑Bus, więc zmiana w Ustawieniach jest od razu widoczna.
pub struct WifiScan {
    monitor: Monitor,
}

impl WifiScan {
    pub fn spawn() -> Self {
        Self {
            monitor: Monitor::spawn(),
        }
    }

    /// Prosi o świeży skan i wkrótce udostępni nową listę.
    pub fn refresh(&self) {
        self.monitor.scan();
    }

    /// Ostatnia znana lista sieci (kopia).
    pub fn current(&self) -> Vec<WifiNetwork> {
        self.monitor.current().wifi
    }

    /// Pełna migawka (urządzenia, znane sieci, łączność).
    pub fn snapshot(&self) -> Snapshot {
        self.monitor.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_no_devices() {
        let snapshot = Snapshot::default();
        assert!(!snapshot.available);
        assert!(!snapshot.is_online());
        assert!(snapshot.ethernet().next().is_none());
    }

    #[test]
    fn security_needs_password() {
        assert!(!Security::Open.needs_password());
        assert!(Security::Wpa2.needs_password());
        assert!(Security::Wpa3.needs_password());
        assert!(!Security::Owe.needs_password());
        assert!(!Security::Wep.is_supported());
    }
}
