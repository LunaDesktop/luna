//! # luna
//!
//! Powłoka środowiska graficznego **Luna** — odpowiednik GNOME Shell, ale zbudowany
//! na libLunie i działający na kompozytorze **Selene**.
//!
//! Moduły rysują się przez `libluna-widgets::PaintCtx`, więc ten sam kod obsługuje
//! podglądy headless (zrzuty do dokumentacji) i sesję na żywo.

pub mod accounts_service;
pub mod apps;
pub mod security;
pub mod bluetooth;
pub mod calendar;
pub mod clock_widget;
pub mod colors;
pub mod dbus_notifications;
pub mod default_apps;
pub mod equalizer;
pub mod keyring;
pub mod launcher_conf;
pub mod media;
pub mod network;
pub mod notifications;
pub mod osd;
pub mod pam;
pub mod panel;
pub mod polkit;
pub mod power;
pub mod power_profile;
pub mod printers;
pub mod sessions;
pub mod system;
pub mod theme_schedule;
pub mod toplevels;
pub mod tray;
pub mod volume;
pub mod vpn;
pub mod workspaces;
