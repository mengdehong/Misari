use anyhow::{Context, Result};
use smithay_client_toolkit::{
    delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use std::collections::BTreeSet;
use wayland_client::{
    Connection, EventQueue, QueueHandle, globals::registry_queue_init, protocol::wl_output,
};

struct State {
    registry: RegistryState,
    outputs: OutputState,
    dirty: bool,
}

pub struct Desktop {
    pub connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Desktop {
    pub fn connect() -> Result<Self> {
        let connection =
            Connection::connect_to_env().context("connecting to Wayland for outputs")?;
        let (globals, queue) = registry_queue_init(&connection)?;
        let state = State {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &queue.handle()),
            dirty: true,
        };
        let mut desktop = Self {
            connection,
            queue,
            state,
        };
        desktop
            .queue
            .roundtrip(&mut desktop.state)
            .context("initializing Wayland outputs")?;
        desktop
            .queue
            .roundtrip(&mut desktop.state)
            .context("initializing Wayland outputs")?;
        Ok(desktop)
    }

    pub fn dispatch(&mut self) -> Result<Option<BTreeSet<String>>> {
        self.queue.dispatch_pending(&mut self.state)?;
        if !std::mem::take(&mut self.state.dirty) {
            return Ok(None);
        }
        Ok(Some(
            self.state
                .outputs
                .outputs()
                .filter_map(|output| self.state.outputs.info(&output)?.name)
                .collect(),
        ))
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.dirty = true;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.dirty = true;
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.dirty = true;
    }
}
delegate_output!(State);
delegate_registry!(State);
impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}
