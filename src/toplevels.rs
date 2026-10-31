//! Klient `zwlr_foreign_toplevel_management` — lista okien dla paska (taskbar).
//!
//! Powłoka otwiera **własne** połączenie Wayland (niezależne od pętli `libluna-platform`),
//! binduje menedżera `zwlr_foreign_toplevel` i w osobnym wątku śledzi uchwyty okien.
//! UI (wątek rysowania) czyta gotową listę przez [`Toplevels::tasks`], a kliknięcia
//! tłumaczy na [`Toplevels::activate`] / [`Toplevels::close`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1 as hdl, zwlr_foreign_toplevel_manager_v1 as mgr,
};

/// Stany okna (`zwlr_foreign_toplevel_handle_v1.state`).
pub const ST_MAXIMIZED: u32 = 0;
pub const ST_MINIMIZED: u32 = 1;
pub const ST_ACTIVATED: u32 = 2;
pub const ST_FULLSCREEN: u32 = 3;

/// Migawka okna dla UI.
#[derive(Debug, Clone, Default)]
pub struct Task {
    pub title: String,
    pub app_id: String,
    pub active: bool,
    /// Czy okno jest zminimalizowane (wtedy menu pokazuje „Pokaż” zamiast „Schowaj”).
    pub minimized: bool,
}

#[derive(Default)]
struct Entry {
    title: String,
    app_id: String,
    states: Vec<u32>,
    proxy: Option<hdl::ZwlrForeignToplevelHandleV1>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    order: Vec<u64>,
    map: HashMap<u64, Entry>,
    seat: Option<wl_seat::WlSeat>,
}

struct DispatchState {
    shared: Arc<Mutex<Shared>>,
    waker: Option<libluna_platform::WakeHandle>,
}

impl DispatchState {
    fn wake(&self) {
        if let Some(w) = &self.waker {
            w.wake();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for DispatchState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for DispatchState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<mgr::ZwlrForeignToplevelManagerV1, ()> for DispatchState {
    fn event(
        state: &mut Self,
        _mgr: &mgr::ZwlrForeignToplevelManagerV1,
        event: mgr::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let mgr::Event::Toplevel { toplevel } = event {
            let id = toplevel.id().protocol_id() as u64;
            let mut s = state.shared.lock().unwrap();
            s.order.push(id);
            s.map.insert(
                id,
                Entry {
                    proxy: Some(toplevel),
                    ..Default::default()
                },
            );
            drop(s);
            state.wake();
        }
    }

    // Event `new_id` (uchwyt okna) musi mieć dane.
    wayland_client::event_created_child!(DispatchState, mgr::ZwlrForeignToplevelManagerV1, [
        0 => (hdl::ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<hdl::ZwlrForeignToplevelHandleV1, ()> for DispatchState {
    fn event(
        state: &mut Self,
        handle: &hdl::ZwlrForeignToplevelHandleV1,
        event: hdl::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id().protocol_id() as u64;
        let mut s = state.shared.lock().unwrap();
        let Some(e) = s.map.get_mut(&id) else {
            return;
        };
        match event {
            hdl::Event::Title { title } => e.title = title,
            hdl::Event::AppId { app_id } => e.app_id = app_id,
            hdl::Event::State { state } => e.states = decode_states(&state),
            hdl::Event::Closed => e.closed = true,
            _ => {}
        }
        drop(s);
        state.wake();
    }
}

/// Powłoka połączona z listą okien kompozytora.
pub struct Toplevels {
    shared: Arc<Mutex<Shared>>,
    conn: Connection,
}

impl Toplevels {
    /// Łączy się z kompozytorem i uruchamia wątek śledzący okna.
    /// `None`, gdy nie ma kompozytora albo brakuje protokołu.
    ///
    /// `waker` (opcjonalny) budzi pętlę rysowania, gdy lista okien się zmieni —
    /// bez tego panel nie odświeżał się po otwarciu programu i jego przycisk
    /// w pasku zadań pojawiał się dopiero przy następnej zmianie zegara.
    pub fn spawn(waker: Option<libluna_platform::WakeHandle>) -> Option<Self> {
        // Klient musi mieć `XDG_RUNTIME_DIR`, żeby znaleźć gniazdo kompozytora
        // (menedżer sesji zwykle je ustawia; gdy brak — wybieramy je jak kompozytor).
        let _ = libluna_core::runtime::ensure_runtime_dir();
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<DispatchState>(&conn).ok()?;
        let qh = queue.handle();

        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = DispatchState {
            shared: Arc::clone(&shared),
            waker,
        };

        let _manager = globals
            .bind::<mgr::ZwlrForeignToplevelManagerV1, DispatchState, ()>(&qh, 1..=3, ())
            .ok()?;
        if let Ok(seat) = globals.bind::<wl_seat::WlSeat, DispatchState, ()>(&qh, 1..=1, ()) {
            shared.lock().unwrap().seat = Some(seat);
        }

        std::thread::spawn(move || loop {
            if queue.blocking_dispatch(&mut state).is_err() {
                break;
            }
        });

        Some(Self { shared, conn })
    }

    fn live_ids(s: &Shared) -> Vec<u64> {
        s.order
            .iter()
            .copied()
            .filter(|id| s.map.get(id).map(|e| !e.closed).unwrap_or(false))
            .collect()
    }

    /// Bieżąca lista okien (kolejność jak w kompozytorze).
    pub fn tasks(&self) -> Vec<Task> {
        let s = self.shared.lock().unwrap();
        Self::live_ids(&s)
            .iter()
            .filter_map(|id| s.map.get(id))
            .map(|e| Task {
                title: e.title.clone(),
                app_id: e.app_id.clone(),
                active: e.states.contains(&ST_ACTIVATED),
                minimized: e.states.contains(&ST_MINIMIZED),
            })
            .collect()
    }

    /// Prosi kompozytor o aktywowanie okna o indeksie `index`.
    /// Jeśli okno jest już aktywne — minimalizuje je (klasyczne zachowanie paska zadań).
    pub fn activate(&self, index: usize) {
        {
            let s = self.shared.lock().unwrap();
            let live = Self::live_ids(&s);
            if let (Some(id), Some(seat)) = (live.get(index), s.seat.as_ref()) {
                if let Some(entry) = s.map.get(id) {
                    if let Some(h) = entry.proxy.as_ref() {
                        if entry.states.contains(&ST_ACTIVATED) {
                            let _ = h.set_minimized();
                        } else {
                            let _ = h.activate(seat);
                        }
                    }
                }
            }
        }
        let _ = self.conn.flush();
    }

    /// Prosi kompozytor o zminimalizowanie okna o indeksie `index`.
    pub fn minimize(&self, index: usize) {
        {
            let s = self.shared.lock().unwrap();
            let live = Self::live_ids(&s);
            if let Some(id) = live.get(index) {
                if let Some(h) = s.map.get(id).and_then(|e| e.proxy.as_ref()) {
                    let _ = h.set_minimized();
                }
            }
        }
        let _ = self.conn.flush();
    }

    /// Pokazuje okno: zdejmuje minimalizację i aktywuje (bez ukrywania, gdy już aktywne).
    pub fn show(&self, index: usize) {
        {
            let s = self.shared.lock().unwrap();
            let live = Self::live_ids(&s);
            if let (Some(id), Some(seat)) = (live.get(index), s.seat.as_ref()) {
                if let Some(entry) = s.map.get(id) {
                    if let Some(h) = entry.proxy.as_ref() {
                        if entry.states.contains(&ST_MINIMIZED) {
                            let _ = h.unset_minimized();
                        }
                        let _ = h.activate(seat);
                    }
                }
            }
        }
        let _ = self.conn.flush();
    }

    /// Prosi kompozytor o zamknięcie okna o indeksie `index`.
    pub fn close(&self, index: usize) {
        {
            let s = self.shared.lock().unwrap();
            let live = Self::live_ids(&s);
            if let Some(id) = live.get(index) {
                if let Some(h) = s.map.get(id).and_then(|e| e.proxy.as_ref()) {
                    let _ = h.close();
                }
            }
        }
        let _ = self.conn.flush();
    }
}

/// Dekoduje tablicę `state` (każdy stan to 4-bajtowe `u32`, kolejność natywna).
fn decode_states(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .filter_map(|c| c.try_into().ok())
        .map(u32::from_ne_bytes)
        .collect()
}
