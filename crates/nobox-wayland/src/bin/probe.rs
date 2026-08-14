//! Enumerate the globals advertised by a Wayland compositor.

use anyhow::Result;
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};

struct Probe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

fn main() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let (globals, _event_queue) = registry_queue_init::<Probe>(&connection)?;
    let mut interfaces = globals.contents().clone_list();
    interfaces.sort_by(|left, right| left.interface.cmp(&right.interface));
    for global in interfaces {
        println!("{} {}", global.interface, global.version);
    }
    Ok(())
}
