// SPDX-License-Identifier: MIT
use smithay::backend::input::{self as sm, InputBackend, UnusedEvent};
use crate::{AxisEvent, Backend, ButtonEvent, Input, KeyEvent, MotionEvent};

/// Inputd merges the physical devices into one VT consumer, so this is a
/// logical device, not a fabricated USB identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputdDevice;

impl sm::Device for InputdDevice {
    fn id(&self) -> String { "inputd-vt".into() }
    fn name(&self) -> String { "Inputd keyboard and pointer".into() }
    fn has_capability(&self, capability: sm::DeviceCapability) -> bool {
        matches!(capability, sm::DeviceCapability::Keyboard | sm::DeviceCapability::Pointer)
    }
    fn usb_id(&self) -> Option<(u32, u32)> { None }
    fn syspath(&self) -> Option<std::path::PathBuf> { None }
}

macro_rules! event {
    ($ty:ty) => {
        impl sm::Event<Backend> for $ty {
            fn time(&self) -> u64 { self.time_us }
            fn device(&self) -> InputdDevice { InputdDevice }
        }
    };
}
event!(KeyEvent);
event!(MotionEvent);
event!(ButtonEvent);
event!(AxisEvent);

impl sm::KeyboardKeyEvent<Backend> for KeyEvent {
    fn key_code(&self) -> sm::Keycode { (self.code + 8).into() }
    fn state(&self) -> sm::KeyState {
        if self.pressed { sm::KeyState::Pressed } else { sm::KeyState::Released }
    }
    fn count(&self) -> u32 { self.count }
}

impl sm::PointerButtonEvent<Backend> for ButtonEvent {
    fn button_code(&self) -> u32 { self.code }
    fn state(&self) -> sm::ButtonState {
        if self.pressed { sm::ButtonState::Pressed } else { sm::ButtonState::Released }
    }
}

impl sm::AbsolutePositionEvent<Backend> for MotionEvent {
    fn x(&self) -> f64 { f64::from(self.x) }
    fn y(&self) -> f64 { f64::from(self.y) }
    fn x_transformed(&self, width: i32) -> f64 {
        f64::from(self.x) * f64::from(width.max(0)) / f64::from(self.width.max(1))
    }
    fn y_transformed(&self, height: i32) -> f64 {
        f64::from(self.y) * f64::from(height.max(0)) / f64::from(self.height.max(1))
    }
}
impl sm::PointerMotionAbsoluteEvent<Backend> for MotionEvent {}

impl sm::PointerAxisEvent<Backend> for AxisEvent {
    fn amount(&self, _axis: sm::Axis) -> Option<f64> { None }
    fn amount_v120(&self, axis: sm::Axis) -> Option<f64> {
        Some(f64::from(match axis {
            sm::Axis::Horizontal => self.horizontal,
            sm::Axis::Vertical => self.vertical,
        }) * 120.0)
    }
    fn source(&self) -> sm::AxisSource { sm::AxisSource::Wheel }
    fn relative_direction(&self, _axis: sm::Axis) -> sm::AxisRelativeDirection {
        sm::AxisRelativeDirection::Identical
    }
}

impl Input {
    pub fn into_smithay(self) -> sm::InputEvent<Backend> {
        match self {
            Self::Key(event) => sm::InputEvent::Keyboard { event },
            Self::Motion(event) => sm::InputEvent::PointerMotionAbsolute { event },
            Self::Button(event) => sm::InputEvent::PointerButton { event },
            Self::Axis(event) => sm::InputEvent::PointerAxis { event },
        }
    }
}

impl InputBackend for Backend {
    type Device = InputdDevice;
    type KeyboardKeyEvent = KeyEvent;
    type PointerAxisEvent = AxisEvent;
    type PointerButtonEvent = ButtonEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = MotionEvent;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = UnusedEvent;
}
