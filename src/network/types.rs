//! Typy warstwy sieci (niezależne od backendu).
//!
//! Ustawienia i panel nie wołają D-Busa bezpośrednio — dostają [`Snapshot`]
//! z [`super::Monitor`]. Dzięki temu backend można podmienić (NetworkManager,
//! iwd, connman) bez zmian w UI.

use std::net::Ipv4Addr;

/// Rodzaj urządzenia sieciowego.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Ethernet,
    Wifi,
    Other,
}

/// Stan urządzenia (mapowany z `NMDeviceState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Unknown,
    Unmanaged,
    Unavailable,
    Disconnected,
    Connecting,
    Authenticating,
    IpConfig,
    Activated,
    Deactivating,
    Failed,
}

impl DeviceState {
    pub fn from_nm(state: u32) -> Self {
        match state {
            10 => Self::Unmanaged,
            20 => Self::Unavailable,
            30 => Self::Disconnected,
            40 | 50 => Self::Connecting,
            60 => Self::Authenticating,
            70 | 80 | 90 => Self::IpConfig,
            100 => Self::Activated,
            110 => Self::Deactivating,
            120 => Self::Failed,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Unmanaged => "Unmanaged",
            Self::Unavailable => "Unavailable",
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting…",
            Self::Authenticating => "Authenticating…",
            Self::IpConfig => "Configuring address…",
            Self::Activated => "Connected",
            Self::Deactivating => "Disconnecting…",
            Self::Failed => "Failed",
        }
    }

    pub fn is_connected(self) -> bool {
        matches!(self, Self::Activated | Self::IpConfig)
    }

    pub fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Authenticating | Self::IpConfig | Self::Deactivating
        )
    }
}

/// Stan łączności z internetem (`NMConnectivityState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connectivity {
    #[default]
    Unknown,
    None,
    Portal,
    Limited,
    Full,
}

impl Connectivity {
    pub fn from_nm(state: u32) -> Self {
        match state {
            1 => Self::None,
            2 => Self::Portal,
            3 => Self::Limited,
            4 => Self::Full,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::None => "No internet",
            Self::Portal => "Login required",
            Self::Limited => "Limited",
            Self::Full => "Connected",
        }
    }
}

/// Zabezpieczenie sieci Wi-Fi (na podstawie flag AccessPoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    Wep,
    Wpa2,
    Wpa3,
    Enterprise,
    Owe,
}

impl Security {
    const PRIVACY: u32 = 0x1;
    const KEY_MGMT_PSK: u32 = 0x100;
    const KEY_MGMT_802_1X: u32 = 0x200;
    const KEY_MGMT_SAE: u32 = 0x400;
    const KEY_MGMT_OWE: u32 = 0x800;

    /// Wylicza zabezpieczenie z `Flags`, `WpaFlags` i `RsnFlags` punktu dostępowego.
    pub fn from_flags(flags: u32, wpa: u32, rsn: u32) -> Self {
        let bits = wpa | rsn;
        if flags & Self::PRIVACY == 0 {
            Self::Open
        } else if bits & Self::KEY_MGMT_SAE != 0 {
            Self::Wpa3
        } else if bits & Self::KEY_MGMT_OWE != 0 {
            Self::Owe
        } else if bits & Self::KEY_MGMT_802_1X != 0 {
            Self::Enterprise
        } else if bits & Self::KEY_MGMT_PSK != 0 {
            Self::Wpa2
        } else {
            Self::Wep
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Wep => "WEP",
            Self::Wpa2 => "WPA2",
            Self::Wpa3 => "WPA3",
            Self::Enterprise => "802.1X",
            Self::Owe => "OWE",
        }
    }

    /// Czy do połączenia trzeba podać hasło.
    pub fn needs_password(self) -> bool {
        matches!(self, Self::Wep | Self::Wpa2 | Self::Wpa3 | Self::Enterprise)
    }

    /// `key-mgmt` dla NetworkManagera; `None` = sieć otwarta/OWE.
    pub fn key_mgmt(self) -> Option<&'static str> {
        match self {
            Self::Wpa2 => Some("wpa-psk"),
            Self::Wpa3 => Some("sae"),
            Self::Owe => Some("owe"),
            _ => None,
        }
    }

    /// Czy backend potrafi obsłużyć ten typ zabezpieczenia.
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Wep | Self::Enterprise)
    }
}

/// Metoda konfiguracji IPv4 w profilu połączenia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ipv4Method {
    /// Adres z DHCP.
    #[default]
    Auto,
    /// Adres wpisany ręcznie.
    Manual,
    /// IPv4 wyłączone w profilu.
    Disabled,
    /// Inna metoda (`shared`, `link-local`, …) — nie edytujemy jej z Ustawień.
    Other,
}

impl Ipv4Method {
    pub fn from_nm(method: &str) -> Self {
        match method {
            "auto" => Self::Auto,
            "manual" => Self::Manual,
            "disabled" | "ignore" => Self::Disabled,
            _ => Self::Other,
        }
    }

    pub fn as_nm(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
            Self::Disabled | Self::Other => "disabled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (DHCP)",
            Self::Manual => "Manual",
            Self::Disabled => "Disabled",
            Self::Other => "Other",
        }
    }
}

/// Konfiguracja IPv4 zapisanego profilu przewodowego.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiredConfig {
    pub method: Ipv4Method,
    /// `adres/prefix` (puste przy DHCP).
    pub address: String,
    pub gateway: String,
    /// Serwery DNS w kolejności wpisania.
    pub dns: Vec<String>,
    /// Ścieżka D-Bus profilu (`None` = profil jeszcze nie istnieje).
    pub profile: Option<String>,
    pub autoconnect: bool,
}

impl Default for WiredConfig {
    /// Nowy profil łączy się automatycznie (tak zachowuje się NetworkManager).
    fn default() -> Self {
        Self {
            method: Ipv4Method::Auto,
            address: String::new(),
            gateway: String::new(),
            dns: Vec::new(),
            profile: None,
            autoconnect: true,
        }
    }
}

impl WiredConfig {
    pub fn is_dhcp(&self) -> bool {
        self.method != Ipv4Method::Manual
    }

    /// Sprawdza, czy ręczna konfiguracja jest kompletna i poprawna.
    pub fn validate(&self) -> Result<(), NetworkError> {
        if self.is_dhcp() {
            return Ok(());
        }
        let Some((_, prefix)) = split_cidr(self.address.trim()) else {
            return Err(NetworkError(
                "Enter the address with a prefix, e.g. 192.168.1.20/24".to_owned(),
            ));
        };
        if prefix > 32 {
            return Err(NetworkError(
                "The prefix must be between 0 and 32".to_owned(),
            ));
        }
        let gateway = self.gateway.trim();
        if !gateway.is_empty() && gateway.parse::<Ipv4Addr>().is_err() {
            return Err(NetworkError(format!("Invalid gateway: {gateway}")));
        }
        for dns in self
            .dns
            .iter()
            .map(|dns| dns.trim())
            .filter(|dns| !dns.is_empty())
        {
            if dns.parse::<Ipv4Addr>().is_err() {
                return Err(NetworkError(format!("Invalid DNS server: {dns}")));
            }
        }
        Ok(())
    }
}

/// Dzieli `adres/prefix`; brak prefixu oznacza `/24`.
pub fn split_cidr(value: &str) -> Option<(Ipv4Addr, u8)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (address, prefix) = match value.split_once('/') {
        Some((address, prefix)) => (address, prefix.parse::<u8>().ok()?),
        None => (value, 24),
    };
    Some((address.parse().ok()?, prefix))
}

/// Zapisana sieć Wi-Fi (profil NetworkManagera).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownWifi {
    pub ssid: String,
    /// Ścieżka D-Bus profilu (do usunięcia / zmiany autoconnect).
    pub profile: String,
    pub autoconnect: bool,
}

/// Urządzenie sieciowe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub interface: String,
    pub kind: DeviceKind,
    pub state: DeviceState,
    pub driver: String,
    pub hw_address: String,
    /// Nazwa aktywnego połączenia (`None` = brak).
    pub connection: Option<String>,
    /// Adres IPv4 w formacie `adres/prefix`.
    pub ip4: Option<String>,
    /// Konfiguracja zapisanego profilu (tylko Ethernet).
    pub wired: Option<WiredConfig>,
    /// Połączenie taryfowane (hotspot, limit danych).
    pub metered: bool,
}

/// Sieć Wi-Fi widziana przez urządzenie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub security: Security,
    pub active: bool,
    /// Ścieżka D-Bus urządzenia Wi-Fi (do aktywacji połączenia).
    pub device: String,
    /// Nazwa interfejsu (np. `wlp12s0f3u2`).
    pub device_name: String,
    pub ap_path: String,
    pub frequency: u32,
    /// Czy mamy zapisany profil tej sieci.
    pub known: bool,
}

/// Migawka stanu sieci dla UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Czy NetworkManager odpowiedział.
    pub available: bool,
    pub error: Option<String>,
    pub wifi_enabled: bool,
    pub connectivity: Connectivity,
    pub devices: Vec<Device>,
    pub wifi: Vec<WifiNetwork>,
    /// Zapisane profile Wi-Fi (także sieci poza zasięgiem).
    pub known_wifi: Vec<KnownWifi>,
}

impl Snapshot {
    pub fn ethernet(&self) -> impl Iterator<Item = &Device> {
        self.devices
            .iter()
            .filter(|device| device.kind == DeviceKind::Ethernet)
    }

    pub fn wifi_devices(&self) -> impl Iterator<Item = &Device> {
        self.devices
            .iter()
            .filter(|device| device.kind == DeviceKind::Wifi)
    }

    pub fn active_wifi(&self) -> Option<&WifiNetwork> {
        self.wifi.iter().find(|network| network.active)
    }

    /// Czy sieć o tym SSID ma zapisany profil.
    pub fn is_known(&self, ssid: &str) -> bool {
        self.known_wifi.iter().any(|known| known.ssid == ssid)
    }

    /// Czy jakiekolwiek połączenie jest aktywne.
    pub fn is_online(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.state.is_connected())
    }
}

/// Opis błędu z backendu sieci.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkError(pub String);

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NetworkError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_from_nm_flags() {
        assert_eq!(Security::from_flags(0, 0, 0), Security::Open);
        let privacy = 1;
        assert_eq!(Security::from_flags(privacy, 0, 0), Security::Wep);
        assert_eq!(
            Security::from_flags(privacy, 0, Security::KEY_MGMT_PSK),
            Security::Wpa2
        );
        assert_eq!(
            Security::from_flags(privacy, 0, Security::KEY_MGMT_SAE),
            Security::Wpa3
        );
        assert_eq!(
            Security::from_flags(privacy, 0, Security::KEY_MGMT_802_1X),
            Security::Enterprise
        );
        assert_eq!(
            Security::from_flags(privacy, Security::KEY_MGMT_OWE, 0),
            Security::Owe
        );
    }

    #[test]
    fn device_states_and_connectivity() {
        assert_eq!(DeviceState::from_nm(100), DeviceState::Activated);
        assert!(DeviceState::from_nm(100).is_connected());
        assert!(DeviceState::from_nm(70).is_busy());
        assert_eq!(Connectivity::from_nm(4), Connectivity::Full);
        assert_eq!(Connectivity::from_nm(99), Connectivity::Unknown);
    }

    #[test]
    fn cidr_parsing_and_wired_validation() {
        assert_eq!(
            split_cidr("10.0.0.5/8"),
            Some((Ipv4Addr::new(10, 0, 0, 5), 8))
        );
        assert_eq!(
            split_cidr("192.168.1.7"),
            Some((Ipv4Addr::new(192, 168, 1, 7), 24))
        );
        assert!(split_cidr("nope").is_none());
        assert!(split_cidr("").is_none());

        let dhcp = WiredConfig::default();
        assert!(dhcp.is_dhcp());
        assert!(dhcp.validate().is_ok());

        let mut manual = WiredConfig {
            method: Ipv4Method::Manual,
            ..WiredConfig::default()
        };
        assert!(manual.validate().is_err());
        manual.address = "192.168.1.20/24".to_owned();
        manual.gateway = "192.168.1.1".to_owned();
        manual.dns = vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()];
        assert!(manual.validate().is_ok());
        manual.dns.push("nie-dns".to_owned());
        assert!(manual.validate().is_err());
    }

    #[test]
    fn ipv4_method_round_trip() {
        for (name, method) in [
            ("auto", Ipv4Method::Auto),
            ("manual", Ipv4Method::Manual),
            ("disabled", Ipv4Method::Disabled),
            ("shared", Ipv4Method::Other),
        ] {
            assert_eq!(Ipv4Method::from_nm(name), method);
        }
        assert_eq!(Ipv4Method::Manual.as_nm(), "manual");
    }
}
