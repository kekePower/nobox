//! Bounded tablet-v2 protocol adapter, including pads and removal lifecycle.

use std::collections::{HashMap, HashSet};

use smithay::{
    backend::input::{ButtonState, TabletToolCapabilities, TabletToolDescriptor, TabletToolType},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource as _, Weak,
        backend::GlobalId, protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Serial},
    wayland::tablet_manager::TabletDescriptor,
};
use wayland_protocols::wp::tablet::zv2::server::{
    zwp_tablet_manager_v2::{self, ZwpTabletManagerV2},
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};

pub(crate) const VERSION: u32 = 1;
pub(crate) const MAX_PADS: usize = 16;
pub(crate) const MAX_PAD_BUTTONS: u32 = 256;
pub(crate) const MAX_PAD_GROUPS: usize = 16;
pub(crate) const MAX_PAD_RINGS: usize = 16;
pub(crate) const MAX_PAD_STRIPS: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PadGroupDescriptor {
    pub(crate) index: u32,
    pub(crate) buttons: Vec<u32>,
    pub(crate) rings: Vec<u32>,
    pub(crate) strips: Vec<u32>,
    pub(crate) modes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PadDescriptor {
    pub(crate) id: String,
    pub(crate) path: Option<String>,
    pub(crate) buttons: u32,
    pub(crate) groups: Vec<PadGroupDescriptor>,
    pub(crate) tablet_id: Option<String>,
}

impl PadDescriptor {
    pub(crate) fn bounded(mut self) -> Self {
        self.id.truncate(256);
        if let Some(path) = &mut self.path {
            path.truncate(4096);
        }
        self.buttons = self.buttons.min(MAX_PAD_BUTTONS);
        self.groups.truncate(MAX_PAD_GROUPS);
        for group in &mut self.groups {
            group.buttons.retain(|button| *button < self.buttons);
            group
                .buttons
                .truncate(usize::try_from(MAX_PAD_BUTTONS).unwrap_or(256));
            group.rings.truncate(MAX_PAD_RINGS);
            group.strips.truncate(MAX_PAD_STRIPS);
            group.modes = group.modes.clamp(1, 64);
        }
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PadAxisSource {
    Unknown,
    Finger,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PadEvent {
    Button {
        button: u32,
        state: ButtonState,
        group: u32,
        mode: u32,
        time: u32,
    },
    Ring {
        ring: u32,
        position: f64,
        source: PadAxisSource,
        group: u32,
        mode: u32,
        time: u32,
    },
    Strip {
        strip: u32,
        position: f64,
        source: PadAxisSource,
        group: u32,
        mode: u32,
        time: u32,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ToolAction {
    Axis,
    ProximityIn,
    ProximityOut,
    TipDown,
    TipUp,
    Button { button: u32, state: ButtonState },
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ToolAxes {
    pub(crate) pressure: Option<f64>,
    pub(crate) distance: Option<f64>,
    pub(crate) tilt: Option<(f64, f64)>,
    pub(crate) rotation: Option<f64>,
    pub(crate) slider: Option<f64>,
    pub(crate) wheel: Option<(f64, i32)>,
}

pub(crate) trait TabletHandler {
    fn tablet_state(&mut self) -> &mut TabletState;
    fn tablet_cursor_request(
        &mut self,
        tool: &TabletToolDescriptor,
        serial: u32,
        surface: Option<WlSurface>,
        hotspot: Point<i32, Logical>,
        resource: &ZwpTabletToolV2,
    );
}

#[derive(Debug)]
pub(crate) struct TabletState {
    _global: GlobalId,
    seats: Vec<Weak<ZwpTabletSeatV2>>,
    tablets: HashMap<String, TabletDevice>,
    tools: HashMap<TabletToolDescriptor, ToolDevice>,
    pads: HashMap<String, PadDevice>,
}

#[derive(Debug)]
struct TabletDevice {
    descriptor: TabletDescriptor,
    instances: Vec<Weak<ZwpTabletV2>>,
}

#[derive(Debug)]
struct ToolDevice {
    instances: Vec<Weak<ZwpTabletToolV2>>,
    focus: Option<ToolFocus>,
    tablet_id: Option<String>,
    down: bool,
    pressed_buttons: HashSet<u32>,
}

#[derive(Debug)]
struct ToolFocus {
    surface: WlSurface,
    origin: Point<f64, Logical>,
    tablet_id: String,
    serial: Serial,
}

#[derive(Debug)]
struct PadDevice {
    descriptor: PadDescriptor,
    instances: Vec<PadInstance>,
    focus: Option<WlSurface>,
    group_modes: HashMap<u32, u32>,
}

#[derive(Debug)]
struct PadInstance {
    pad: Weak<ZwpTabletPadV2>,
    groups: Vec<PadGroupInstance>,
}

#[derive(Debug)]
struct PadGroupInstance {
    index: u32,
    group: Weak<ZwpTabletPadGroupV2>,
    rings: Vec<(u32, Weak<ZwpTabletPadRingV2>)>,
    strips: Vec<(u32, Weak<ZwpTabletPadStripV2>)>,
}

#[derive(Debug)]
pub(crate) struct TabletSeatData;

#[derive(Debug)]
pub(crate) struct TabletData;

#[derive(Debug)]
pub(crate) struct ToolData(pub(crate) TabletToolDescriptor);

#[derive(Debug)]
pub(crate) struct PadData;

#[derive(Debug)]
pub(crate) struct PadGroupData;

#[derive(Debug)]
pub(crate) struct PadRingData;

#[derive(Debug)]
pub(crate) struct PadStripData;

impl TabletState {
    pub(crate) fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwpTabletManagerV2, ()>,
        D: Dispatch<ZwpTabletManagerV2, ()>,
        D: Dispatch<ZwpTabletSeatV2, TabletSeatData>,
        D: Dispatch<ZwpTabletV2, TabletData>,
        D: Dispatch<ZwpTabletToolV2, ToolData>,
        D: Dispatch<ZwpTabletPadV2, PadData>,
        D: Dispatch<ZwpTabletPadGroupV2, PadGroupData>,
        D: Dispatch<ZwpTabletPadRingV2, PadRingData>,
        D: Dispatch<ZwpTabletPadStripV2, PadStripData>,
        D: TabletHandler + 'static,
    {
        let global = display.create_global::<D, ZwpTabletManagerV2, _>(VERSION, ());
        Self {
            _global: global,
            seats: Vec::new(),
            tablets: HashMap::new(),
            tools: HashMap::new(),
            pads: HashMap::new(),
        }
    }

    fn add_seat<D>(&mut self, display: &DisplayHandle, client: &Client, seat: &ZwpTabletSeatV2)
    where
        D: Dispatch<ZwpTabletV2, TabletData>,
        D: Dispatch<ZwpTabletToolV2, ToolData>,
        D: Dispatch<ZwpTabletPadV2, PadData>,
        D: Dispatch<ZwpTabletPadGroupV2, PadGroupData>,
        D: Dispatch<ZwpTabletPadRingV2, PadRingData>,
        D: Dispatch<ZwpTabletPadStripV2, PadStripData>,
        D: 'static,
    {
        self.seats.retain(|seat| seat.upgrade().is_ok());
        for (id, tablet) in &mut self.tablets {
            publish_tablet::<D>(display, client, seat, id, tablet);
        }
        for (descriptor, tool) in &mut self.tools {
            publish_tool::<D>(display, client, seat, descriptor, tool);
        }
        for pad in self.pads.values_mut() {
            publish_pad::<D>(display, client, seat, pad);
        }
        self.seats.push(seat.downgrade());
    }

    pub(crate) fn add_tablet<D>(
        &mut self,
        display: &DisplayHandle,
        id: String,
        mut descriptor: TabletDescriptor,
    ) where
        D: Dispatch<ZwpTabletV2, TabletData> + 'static,
    {
        if self.tablets.contains_key(&id) {
            return;
        }
        descriptor.name.truncate(256);
        let mut tablet = TabletDevice {
            descriptor,
            instances: Vec::new(),
        };
        for seat in self.seats.iter().filter_map(|seat| seat.upgrade().ok()) {
            if let Ok(client) = display.get_client(seat.id()) {
                publish_tablet::<D>(display, &client, &seat, &id, &mut tablet);
            }
        }
        self.tablets.insert(id, tablet);
    }

    pub(crate) fn remove_tablet(&mut self, id: &str, time: u32) {
        self.remove_tools_for_tablet(id, time);
        for pad in self
            .pads
            .values_mut()
            .filter(|pad| pad.descriptor.tablet_id.as_deref() == Some(id))
        {
            leave_pad(pad);
            pad.descriptor.tablet_id = None;
        }
        if let Some(tablet) = self.tablets.remove(id) {
            for instance in tablet
                .instances
                .into_iter()
                .filter_map(|item| item.upgrade().ok())
            {
                instance.removed();
            }
        }
    }

    pub(crate) fn add_pad<D>(&mut self, display: &DisplayHandle, descriptor: PadDescriptor)
    where
        D: Dispatch<ZwpTabletPadV2, PadData>,
        D: Dispatch<ZwpTabletPadGroupV2, PadGroupData>,
        D: Dispatch<ZwpTabletPadRingV2, PadRingData>,
        D: Dispatch<ZwpTabletPadStripV2, PadStripData>,
        D: 'static,
    {
        let descriptor = descriptor.bounded();
        if self.pads.contains_key(&descriptor.id) || self.pads.len() >= MAX_PADS {
            return;
        }
        let mut pad = PadDevice {
            descriptor,
            instances: Vec::new(),
            focus: None,
            group_modes: HashMap::new(),
        };
        for seat in self.seats.iter().filter_map(|seat| seat.upgrade().ok()) {
            if let Ok(client) = display.get_client(seat.id()) {
                publish_pad::<D>(display, &client, &seat, &mut pad);
            }
        }
        self.pads.insert(pad.descriptor.id.clone(), pad);
    }

    pub(crate) fn pair_pad(&mut self, pad_id: &str, tablet_id: Option<String>) {
        let Some(pad) = self.pads.get_mut(pad_id) else {
            return;
        };
        if pad.descriptor.tablet_id != tablet_id {
            leave_pad(pad);
            pad.descriptor.tablet_id = tablet_id;
        }
    }

    pub(crate) fn remove_pad(&mut self, id: &str) {
        if let Some(mut pad) = self.pads.remove(id) {
            leave_pad(&mut pad);
            for instance in pad
                .instances
                .into_iter()
                .filter_map(|item| item.pad.upgrade().ok())
            {
                instance.removed();
            }
        }
    }

    pub(crate) fn add_tool<D>(&mut self, display: &DisplayHandle, descriptor: TabletToolDescriptor)
    where
        D: Dispatch<ZwpTabletToolV2, ToolData> + 'static,
    {
        if self.tools.contains_key(&descriptor) {
            return;
        }
        let mut tool = ToolDevice {
            instances: Vec::new(),
            focus: None,
            tablet_id: None,
            down: false,
            pressed_buttons: HashSet::new(),
        };
        for seat in self.seats.iter().filter_map(|seat| seat.upgrade().ok()) {
            if let Ok(client) = display.get_client(seat.id()) {
                publish_tool::<D>(display, &client, &seat, &descriptor, &mut tool);
            }
        }
        self.tools.insert(descriptor, tool);
    }

    pub(crate) fn tool_count(&self) -> usize {
        self.tools.len()
    }

    pub(crate) fn contains_tool(&self, descriptor: &TabletToolDescriptor) -> bool {
        self.tools.contains_key(descriptor)
    }

    pub(crate) fn tablet_count(&self) -> usize {
        self.tablets.len()
    }

    pub(crate) fn contains_tablet(&self, id: &str) -> bool {
        self.tablets.contains_key(id)
    }

    pub(crate) fn tool_focus(
        &self,
        descriptor: &TabletToolDescriptor,
    ) -> Option<(WlSurface, Serial)> {
        self.tools
            .get(descriptor)?
            .focus
            .as_ref()
            .map(|focus| (focus.surface.clone(), focus.serial))
    }

    pub(crate) fn remove_tools_for_tablet(&mut self, tablet_id: &str, time: u32) {
        let descriptors = self
            .tools
            .iter()
            .filter(|(_, tool)| tool.tablet_id.as_deref() == Some(tablet_id))
            .map(|(descriptor, _)| descriptor.clone())
            .collect::<Vec<_>>();
        for descriptor in descriptors {
            self.remove_tool(&descriptor, time);
        }
    }

    pub(crate) fn remove_tool(&mut self, descriptor: &TabletToolDescriptor, time: u32) {
        let Some(mut tool) = self.tools.remove(descriptor) else {
            return;
        };
        tool_leave(&mut tool, time);
        for instance in tool
            .instances
            .into_iter()
            .filter_map(|item| item.upgrade().ok())
        {
            instance.removed();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tool_event<D>(
        &mut self,
        display: &DisplayHandle,
        descriptor: TabletToolDescriptor,
        tablet_id: String,
        location: Point<f64, Logical>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        serial: Serial,
        time: u32,
        axes: ToolAxes,
        action: ToolAction,
    ) where
        D: Dispatch<ZwpTabletToolV2, ToolData> + 'static,
    {
        self.add_tool::<D>(display, descriptor.clone());
        let Some(mut tool) = self.tools.remove(&descriptor) else {
            return;
        };
        tool.tablet_id = Some(tablet_id.clone());
        if matches!(action, ToolAction::ProximityOut) {
            let old_focus = tool
                .focus
                .as_ref()
                .map(|item| (item.tablet_id.clone(), item.surface.clone()));
            tool_leave(&mut tool, time);
            if let Some((old_tablet_id, surface)) = old_focus {
                self.update_pad_focus(&old_tablet_id, Some(&surface), None, serial);
            }
            self.tools.insert(descriptor, tool);
            return;
        }
        let focus_changed = match (&tool.focus, &focus) {
            (Some(old), Some((surface, _))) => {
                old.surface != *surface || old.tablet_id != tablet_id
            }
            (None, Some(_)) => true,
            (Some(_), None) => true,
            (None, None) => false,
        };
        if focus_changed {
            let old_focus = tool
                .focus
                .as_ref()
                .map(|item| (item.tablet_id.clone(), item.surface.clone()));
            tool_leave(&mut tool, time);
            if let Some((old_tablet_id, surface)) = old_focus {
                self.update_pad_focus(&old_tablet_id, Some(&surface), None, serial);
            }
            if let Some((surface, origin)) = focus {
                tool.focus = Some(ToolFocus {
                    surface: surface.clone(),
                    origin,
                    tablet_id: tablet_id.clone(),
                    serial,
                });
                send_tool_enter(&self.tablets, &mut tool, location, time);
                self.update_pad_focus(&tablet_id, None, Some(&surface), serial);
            }
        }
        let Some(current) = tool.focus.as_ref() else {
            self.tools.insert(descriptor, tool);
            return;
        };
        let Some(instance) = resource_for_surface(&tool.instances, &current.surface) else {
            self.tools.insert(descriptor, tool);
            return;
        };
        send_axes(&instance, axes);
        instance.motion(location.x - current.origin.x, location.y - current.origin.y);
        match action {
            ToolAction::TipDown if !tool.down => {
                instance.down(serial.into());
                tool.down = true;
            }
            ToolAction::TipUp if tool.down => {
                instance.up();
                tool.down = false;
            }
            ToolAction::Button { button, state } => {
                match state {
                    ButtonState::Pressed => {
                        tool.pressed_buttons.insert(button);
                    }
                    ButtonState::Released => {
                        tool.pressed_buttons.remove(&button);
                    }
                }
                instance.button(serial.into(), button, button_state(state));
            }
            ToolAction::Axis
            | ToolAction::ProximityIn
            | ToolAction::TipDown
            | ToolAction::TipUp => {}
            ToolAction::ProximityOut => unreachable!(),
        }
        instance.frame(time);
        self.tools.insert(descriptor, tool);
    }

    pub(crate) fn pad_event(&mut self, id: &str, event: PadEvent, serial: Serial) {
        let Some(pad) = self.pads.get_mut(id) else {
            return;
        };
        let Some(focus) = pad.focus.as_ref() else {
            return;
        };
        let Some(instance) = pad
            .instances
            .iter()
            .find(|instance| instance.pad.id().same_client_as(&focus.id()))
        else {
            return;
        };
        let Some(resource) = instance.pad.upgrade().ok() else {
            return;
        };
        match event {
            PadEvent::Button {
                button,
                state,
                group,
                mode,
                time,
            } => {
                let group_resource = find_group(instance, group);
                resource.button(time, button, pad_button_state(state));
                send_mode_switch(pad, group_resource, group, mode, time, serial);
            }
            PadEvent::Ring {
                ring,
                position,
                source,
                group,
                mode,
                time,
            } => {
                let group_resource = find_group(instance, group);
                let ring = find_ring(instance, group, ring);
                send_mode_switch(pad, group_resource, group, mode, time, serial);
                if let Some(ring) = ring {
                    if matches!(source, PadAxisSource::Finger) {
                        ring.source(zwp_tablet_pad_ring_v2::Source::Finger);
                    }
                    if position < 0.0 {
                        ring.stop();
                    } else {
                        ring.angle(position);
                    }
                    ring.frame(time);
                }
            }
            PadEvent::Strip {
                strip,
                position,
                source,
                group,
                mode,
                time,
            } => {
                let group_resource = find_group(instance, group);
                let strip = find_strip(instance, group, strip);
                send_mode_switch(pad, group_resource, group, mode, time, serial);
                if let Some(strip) = strip {
                    if matches!(source, PadAxisSource::Finger) {
                        strip.source(zwp_tablet_pad_strip_v2::Source::Finger);
                    }
                    if position < 0.0 {
                        strip.stop();
                    } else {
                        strip.position((position.clamp(0.0, 1.0) * 65_535.0).round() as u32);
                    }
                    strip.frame(time);
                }
            }
        }
    }

    fn update_pad_focus(
        &mut self,
        tablet_id: &str,
        old_surface: Option<&WlSurface>,
        new_surface: Option<&WlSurface>,
        serial: Serial,
    ) {
        let Some(tablet) = self.tablets.get(tablet_id) else {
            return;
        };
        for pad in self
            .pads
            .values_mut()
            .filter(|pad| pad.descriptor.tablet_id.as_deref() == Some(tablet_id))
        {
            if let Some(surface) = old_surface {
                for instance in pad
                    .instances
                    .iter()
                    .filter(|item| item.pad.id().same_client_as(&surface.id()))
                {
                    if let Ok(resource) = instance.pad.upgrade() {
                        resource.leave(serial.into(), surface);
                    }
                }
                pad.focus = None;
            }
            if let Some(surface) = new_surface
                && let (Some(pad_resource), Some(tablet_resource)) = (
                    pad.instances
                        .iter()
                        .find(|item| item.pad.id().same_client_as(&surface.id()))
                        .and_then(|item| item.pad.upgrade().ok()),
                    resource_for_surface(&tablet.instances, surface),
                )
            {
                pad_resource.enter(serial.into(), &tablet_resource, surface);
                pad.focus = Some(surface.clone());
            }
        }
    }
}

fn publish_tablet<D>(
    display: &DisplayHandle,
    client: &Client,
    seat: &ZwpTabletSeatV2,
    _id: &str,
    tablet: &mut TabletDevice,
) where
    D: Dispatch<ZwpTabletV2, TabletData> + 'static,
{
    let Ok(resource) =
        client.create_resource::<ZwpTabletV2, _, D>(display, seat.version(), TabletData)
    else {
        return;
    };
    seat.tablet_added(&resource);
    resource.name(tablet.descriptor.name.clone());
    if let Some((product, vendor)) = tablet.descriptor.usb_id {
        resource.id(vendor, product);
    }
    if let Some(path) = tablet
        .descriptor
        .syspath
        .as_ref()
        .and_then(|path| path.to_str())
    {
        resource.path(path.to_owned());
    }
    resource.done();
    tablet.instances.push(resource.downgrade());
}

fn publish_tool<D>(
    display: &DisplayHandle,
    client: &Client,
    seat: &ZwpTabletSeatV2,
    descriptor: &TabletToolDescriptor,
    tool: &mut ToolDevice,
) where
    D: Dispatch<ZwpTabletToolV2, ToolData> + 'static,
{
    let Ok(resource) = client.create_resource::<ZwpTabletToolV2, _, D>(
        display,
        seat.version(),
        ToolData(descriptor.clone()),
    ) else {
        return;
    };
    seat.tool_added(&resource);
    resource._type(tool_type(descriptor.tool_type));
    resource.hardware_serial(
        (descriptor.hardware_serial >> 32) as u32,
        descriptor.hardware_serial as u32,
    );
    resource.hardware_id_wacom(
        (descriptor.hardware_id_wacom >> 32) as u32,
        descriptor.hardware_id_wacom as u32,
    );
    for (flag, capability) in [
        (
            TabletToolCapabilities::TILT,
            zwp_tablet_tool_v2::Capability::Tilt,
        ),
        (
            TabletToolCapabilities::PRESSURE,
            zwp_tablet_tool_v2::Capability::Pressure,
        ),
        (
            TabletToolCapabilities::DISTANCE,
            zwp_tablet_tool_v2::Capability::Distance,
        ),
        (
            TabletToolCapabilities::ROTATION,
            zwp_tablet_tool_v2::Capability::Rotation,
        ),
        (
            TabletToolCapabilities::SLIDER,
            zwp_tablet_tool_v2::Capability::Slider,
        ),
        (
            TabletToolCapabilities::WHEEL,
            zwp_tablet_tool_v2::Capability::Wheel,
        ),
    ] {
        if descriptor.capabilities.contains(flag) {
            resource.capability(capability);
        }
    }
    resource.done();
    tool.instances.push(resource.downgrade());
}

fn publish_pad<D>(
    display: &DisplayHandle,
    client: &Client,
    seat: &ZwpTabletSeatV2,
    pad: &mut PadDevice,
) where
    D: Dispatch<ZwpTabletPadV2, PadData>,
    D: Dispatch<ZwpTabletPadGroupV2, PadGroupData>,
    D: Dispatch<ZwpTabletPadRingV2, PadRingData>,
    D: Dispatch<ZwpTabletPadStripV2, PadStripData>,
    D: 'static,
{
    let Ok(resource) =
        client.create_resource::<ZwpTabletPadV2, _, D>(display, seat.version(), PadData)
    else {
        return;
    };
    seat.pad_added(&resource);
    if let Some(path) = &pad.descriptor.path {
        resource.path(path.clone());
    }
    resource.buttons(pad.descriptor.buttons);
    let mut groups = Vec::new();
    for descriptor in &pad.descriptor.groups {
        let Ok(group) = client.create_resource::<ZwpTabletPadGroupV2, _, D>(
            display,
            seat.version(),
            PadGroupData,
        ) else {
            continue;
        };
        resource.group(&group);
        let mut bytes = Vec::with_capacity(descriptor.buttons.len().saturating_mul(4));
        for button in &descriptor.buttons {
            bytes.extend_from_slice(&button.to_ne_bytes());
        }
        group.buttons(bytes);
        let mut rings = Vec::new();
        for number in &descriptor.rings {
            if let Ok(ring) = client.create_resource::<ZwpTabletPadRingV2, _, D>(
                display,
                seat.version(),
                PadRingData,
            ) {
                group.ring(&ring);
                rings.push((*number, ring.downgrade()));
            }
        }
        let mut strips = Vec::new();
        for number in &descriptor.strips {
            if let Ok(strip) = client.create_resource::<ZwpTabletPadStripV2, _, D>(
                display,
                seat.version(),
                PadStripData,
            ) {
                group.strip(&strip);
                strips.push((*number, strip.downgrade()));
            }
        }
        group.modes(descriptor.modes);
        group.done();
        groups.push(PadGroupInstance {
            index: descriptor.index,
            group: group.downgrade(),
            rings,
            strips,
        });
    }
    resource.done();
    pad.instances.push(PadInstance {
        pad: resource.downgrade(),
        groups,
    });
}

fn resource_for_surface<R: smithay::reexports::wayland_server::Resource>(
    resources: &[Weak<R>],
    surface: &WlSurface,
) -> Option<R> {
    resources
        .iter()
        .find(|resource| resource.id().same_client_as(&surface.id()))
        .and_then(|resource| resource.upgrade().ok())
}

fn send_tool_enter(
    tablets: &HashMap<String, TabletDevice>,
    tool: &mut ToolDevice,
    location: Point<f64, Logical>,
    time: u32,
) {
    let Some(focus) = tool.focus.as_ref() else {
        return;
    };
    let Some(instance) = resource_for_surface(&tool.instances, &focus.surface) else {
        return;
    };
    let Some(tablet) = tablets.get(&focus.tablet_id) else {
        return;
    };
    let Some(tablet_resource) = resource_for_surface(&tablet.instances, &focus.surface) else {
        return;
    };
    instance.proximity_in(focus.serial.into(), &tablet_resource, &focus.surface);
    instance.motion(location.x - focus.origin.x, location.y - focus.origin.y);
    instance.frame(time);
}

fn send_axes(tool: &ZwpTabletToolV2, axes: ToolAxes) {
    if let Some(value) = axes.pressure {
        tool.pressure((value.clamp(0.0, 1.0) * 65_535.0).round() as u32);
    }
    if let Some(value) = axes.distance {
        tool.distance((value.clamp(0.0, 1.0) * 65_535.0).round() as u32);
    }
    if let Some((x, y)) = axes.tilt {
        tool.tilt(x, y);
    }
    if let Some(value) = axes.rotation {
        tool.rotation(value);
    }
    if let Some(value) = axes.slider {
        tool.slider((value.clamp(-1.0, 1.0) * 65_535.0).round() as i32);
    }
    if let Some((degrees, clicks)) = axes.wheel {
        tool.wheel(degrees, clicks);
    }
}

fn tool_leave(tool: &mut ToolDevice, time: u32) {
    let Some(focus) = tool.focus.take() else {
        return;
    };
    if let Some(instance) = resource_for_surface(&tool.instances, &focus.surface) {
        if tool.down {
            instance.up();
        }
        for button in tool.pressed_buttons.drain() {
            instance.button(
                focus.serial.into(),
                button,
                zwp_tablet_tool_v2::ButtonState::Released,
            );
        }
        instance.proximity_out();
        instance.frame(time);
    }
    tool.down = false;
}

fn leave_pad(pad: &mut PadDevice) {
    let Some(surface) = pad.focus.take() else {
        return;
    };
    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
    for instance in pad
        .instances
        .iter()
        .filter(|item| item.pad.id().same_client_as(&surface.id()))
    {
        if let Ok(resource) = instance.pad.upgrade() {
            resource.leave(serial.into(), &surface);
        }
    }
}

fn send_mode_switch(
    pad: &mut PadDevice,
    group: Option<ZwpTabletPadGroupV2>,
    group_index: u32,
    mode: u32,
    time: u32,
    serial: Serial,
) {
    if pad.group_modes.get(&group_index).copied() == Some(mode) {
        return;
    }
    pad.group_modes.insert(group_index, mode);
    if let Some(group) = group {
        group.mode_switch(time, serial.into(), mode);
    }
}

fn find_group(instance: &PadInstance, group: u32) -> Option<ZwpTabletPadGroupV2> {
    instance
        .groups
        .iter()
        .find(|item| item.index == group)?
        .group
        .upgrade()
        .ok()
}

fn find_ring(instance: &PadInstance, group: u32, ring: u32) -> Option<ZwpTabletPadRingV2> {
    instance
        .groups
        .iter()
        .find(|item| item.index == group)?
        .rings
        .iter()
        .find(|(number, _)| *number == ring)?
        .1
        .upgrade()
        .ok()
}

fn find_strip(instance: &PadInstance, group: u32, strip: u32) -> Option<ZwpTabletPadStripV2> {
    instance
        .groups
        .iter()
        .find(|item| item.index == group)?
        .strips
        .iter()
        .find(|(number, _)| *number == strip)?
        .1
        .upgrade()
        .ok()
}

fn tool_type(value: TabletToolType) -> zwp_tablet_tool_v2::Type {
    match value {
        TabletToolType::Pen | TabletToolType::Totem | TabletToolType::Unknown => {
            zwp_tablet_tool_v2::Type::Pen
        }
        TabletToolType::Eraser => zwp_tablet_tool_v2::Type::Eraser,
        TabletToolType::Brush => zwp_tablet_tool_v2::Type::Brush,
        TabletToolType::Pencil => zwp_tablet_tool_v2::Type::Pencil,
        TabletToolType::Airbrush => zwp_tablet_tool_v2::Type::Airbrush,
        TabletToolType::Mouse => zwp_tablet_tool_v2::Type::Mouse,
        TabletToolType::Lens => zwp_tablet_tool_v2::Type::Lens,
    }
}

fn button_state(value: ButtonState) -> zwp_tablet_tool_v2::ButtonState {
    match value {
        ButtonState::Pressed => zwp_tablet_tool_v2::ButtonState::Pressed,
        ButtonState::Released => zwp_tablet_tool_v2::ButtonState::Released,
    }
}

fn pad_button_state(value: ButtonState) -> zwp_tablet_pad_v2::ButtonState {
    match value {
        ButtonState::Pressed => zwp_tablet_pad_v2::ButtonState::Pressed,
        ButtonState::Released => zwp_tablet_pad_v2::ButtonState::Released,
    }
}

impl<D> GlobalDispatch<ZwpTabletManagerV2, (), D> for TabletState
where
    D: GlobalDispatch<ZwpTabletManagerV2, ()>,
    D: Dispatch<ZwpTabletManagerV2, ()>,
    D: TabletHandler + 'static,
{
    fn bind(
        _state: &mut D,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTabletManagerV2>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<ZwpTabletManagerV2, (), D> for TabletState
where
    D: Dispatch<ZwpTabletManagerV2, ()>,
    D: Dispatch<ZwpTabletSeatV2, TabletSeatData>,
    D: Dispatch<ZwpTabletV2, TabletData>,
    D: Dispatch<ZwpTabletToolV2, ToolData>,
    D: Dispatch<ZwpTabletPadV2, PadData>,
    D: Dispatch<ZwpTabletPadGroupV2, PadGroupData>,
    D: Dispatch<ZwpTabletPadRingV2, PadRingData>,
    D: Dispatch<ZwpTabletPadStripV2, PadStripData>,
    D: TabletHandler + 'static,
{
    fn request(
        state: &mut D,
        client: &Client,
        _resource: &ZwpTabletManagerV2,
        request: zwp_tablet_manager_v2::Request,
        _data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_tablet_manager_v2::Request::GetTabletSeat { tablet_seat, seat } => {
                if seat.client().as_ref().map(Client::id) != Some(client.id()) {
                    return;
                }
                let seat = data_init.init(tablet_seat, TabletSeatData);
                state.tablet_state().add_seat::<D>(display, client, &seat);
            }
            zwp_tablet_manager_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

macro_rules! inert_dispatch {
    ($interface:ty, $data:ty, $request:path) => {
        impl<D: TabletHandler + 'static> Dispatch<$interface, $data, D> for TabletState {
            fn request(
                _state: &mut D,
                _client: &Client,
                _resource: &$interface,
                request: <$interface as smithay::reexports::wayland_server::Resource>::Request,
                _data: &$data,
                _display: &DisplayHandle,
                _data_init: &mut DataInit<'_, D>,
            ) {
                match request {
                    $request => {}
                    _ => {}
                }
            }
        }
    };
}

inert_dispatch!(
    ZwpTabletSeatV2,
    TabletSeatData,
    zwp_tablet_seat_v2::Request::Destroy
);
inert_dispatch!(ZwpTabletV2, TabletData, zwp_tablet_v2::Request::Destroy);
inert_dispatch!(ZwpTabletPadV2, PadData, zwp_tablet_pad_v2::Request::Destroy);
inert_dispatch!(
    ZwpTabletPadGroupV2,
    PadGroupData,
    zwp_tablet_pad_group_v2::Request::Destroy
);
inert_dispatch!(
    ZwpTabletPadRingV2,
    PadRingData,
    zwp_tablet_pad_ring_v2::Request::Destroy
);
inert_dispatch!(
    ZwpTabletPadStripV2,
    PadStripData,
    zwp_tablet_pad_strip_v2::Request::Destroy
);

impl<D: TabletHandler + 'static> Dispatch<ZwpTabletToolV2, ToolData, D> for TabletState {
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwpTabletToolV2,
        request: zwp_tablet_tool_v2::Request,
        data: &ToolData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_tablet_tool_v2::Request::SetCursor {
                serial,
                surface,
                hotspot_x,
                hotspot_y,
            } => state.tablet_cursor_request(
                &data.0,
                serial,
                surface,
                (hotspot_x, hotspot_y).into(),
                resource,
            ),
            zwp_tablet_tool_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_pad_descriptors_are_bounded() {
        let descriptor = PadDescriptor {
            id: "x".repeat(500),
            path: Some("p".repeat(5000)),
            buttons: u32::MAX,
            groups: (0..32)
                .map(|index| PadGroupDescriptor {
                    index,
                    buttons: (0..512).collect(),
                    rings: (0..32).collect(),
                    strips: (0..32).collect(),
                    modes: u32::MAX,
                })
                .collect(),
            tablet_id: None,
        }
        .bounded();
        assert_eq!(descriptor.id.len(), 256);
        assert_eq!(descriptor.path.as_ref().map(String::len), Some(4096));
        assert_eq!(descriptor.buttons, MAX_PAD_BUTTONS);
        assert_eq!(descriptor.groups.len(), MAX_PAD_GROUPS);
        assert!(descriptor.groups.iter().all(|group| {
            group.buttons.len() <= 256
                && group.rings.len() <= MAX_PAD_RINGS
                && group.strips.len() <= MAX_PAD_STRIPS
                && group.modes == 64
        }));
    }
}
