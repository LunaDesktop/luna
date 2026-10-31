//! Klient `ext_workspace_v1` — lista przestrzeni roboczych dla paska.
//!
//! Powłoka otwiera **własne** połączenie Wayland (jak dla listy okien) i binduje
//! menedżera przestrzeni. W osobnym wątku śledzi uchwyty (id, nazwa, stan `active`),
//! a UI czyta gotową listę przez [`Workspaces::list`] i przełącza przez
//! [`Workspaces::activate`] — dzięki temu kropki w pasku odzwierciedlają realny stan
//! kompozytora i pozwalają klikać.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1 as group, ext_workspace_handle_v1 as ws,
    ext_workspace_manager_v1 as mgr,
};

/// Migawka przestrzeni roboczej dla UI.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub active: bool,
}

#[derive(Default)]
struct Entry {
    id: String,
    name: String,
    active: bool,
    proxy: Option<ws::ExtWorkspaceHandleV1>,
    removed: bool,
}

#[derive(Default)]
struct Shared {
    order: Vec<u64>,
    map: HashMap<u64, Entry>,
}

struct DispatchState {
    shared: Arc<Mutex<Shared>>,
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

impl Dispatch<group::ExtWorkspaceGroupHandleV1, ()> for DispatchState {
    fn event(
        _: &mut Self,
        _: &group::ExtWorkspaceGroupHandleV1,
        _: group::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<mgr::ExtWorkspaceManagerV1, ()> for DispatchState {
    fn event(
        state: &mut Self,
        _mgr: &mgr::ExtWorkspaceManagerV1,
        event: mgr::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let mgr::Event::Workspace { workspace } = event {
            let id = workspace.id().protocol_id() as u64;
            let mut s = state.shared.lock().unwrap();
            s.order.push(id);
            s.map.insert(
                id,
                Entry {
                    proxy: Some(workspace),
                    ..Default::default()
                },
            );
        }
    }

    // Eventy `new_id` (uchwyt grupy i uchwyty przestrzeni) muszą mieć dane.
    wayland_client::event_created_child!(DispatchState, mgr::ExtWorkspaceManagerV1, [
        0 => (group::ExtWorkspaceGroupHandleV1, ()),
        1 => (ws::ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ws::ExtWorkspaceHandleV1, ()> for DispatchState {
    fn event(
        state: &mut Self,
        handle: &ws::ExtWorkspaceHandleV1,
        event: ws::Event,
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
            ws::Event::Id { id } => e.id = id,
            ws::Event::Name { name } => e.name = name,
            ws::Event::State { state } => {
                e.active = matches!(state, WEnum::Value(ws::State::Active));
            }
            ws::Event::Removed => e.removed = true,
            _ => {}
        }
    }
}

/// Powłoka połączona z listą przestrzeni roboczych kompozytora.
pub struct Workspaces {
    shared: Arc<Mutex<Shared>>,
    conn: Connection,
}

impl Workspaces {
    /// Łączy się z kompozytorem i uruchamia wątek śledzący przestrzenie.
    /// `None`, gdy nie ma kompozytora albo brakuje protokołu.
    pub fn spawn() -> Option<Self> {
        let _ = libluna_core::runtime::ensure_runtime_dir();
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<DispatchState>(&conn).ok()?;
        let qh = queue.handle();

        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = DispatchState {
            shared: Arc::clone(&shared),
        };

        let _manager = globals
            .bind::<mgr::ExtWorkspaceManagerV1, DispatchState, ()>(&qh, 1..=1, ())
            .ok()?;

        std::thread::spawn(move || loop {
            if queue.blocking_dispatch(&mut state).is_err() {
                break;
            }
        });

        Some(Self { shared, conn })
    }

    /// Bieżąca lista przestrzeni (kolejność jak w kompozytorze).
    pub fn list(&self) -> Vec<Workspace> {
        let s = self.shared.lock().unwrap();
        s.order
            .iter()
            .filter_map(|id| s.map.get(id))
            .filter(|e| !e.removed)
            .map(|e| Workspace {
                id: e.id.clone(),
                name: e.name.clone(),
                active: e.active,
            })
            .collect()
    }

    /// Prosi kompozytor o aktywowanie przestrzeni o indeksie `index`.
    pub fn activate(&self, index: usize) {
        {
            let s = self.shared.lock().unwrap();
            if let Some(id) = s.order.get(index) {
                if let Some(h) = s.map.get(id).and_then(|e| e.proxy.as_ref()) {
                    let _ = h.activate();
                }
            }
        }
        let _ = self.conn.flush();
    }
}
