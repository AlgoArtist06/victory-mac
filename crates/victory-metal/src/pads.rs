//! Game controllers paired with the Mac (Apple's GameController framework),
//! read as the console's pads.

use objc2_game_controller::GCController;

/// One controller's state: buttons, triggers 0 to 1, sticks -1 to 1 (up
/// positive), as the console's pads report them.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Pad {
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub menu: bool,
    pub view: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub left_shoulder: bool,
    pub right_shoulder: bool,
    pub left_stick: bool,
    pub right_stick: bool,
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub left_x: f32,
    pub left_y: f32,
    pub right_x: f32,
    pub right_y: f32,
}

/// The `slot`th connected controller with a full gamepad, if any.
pub fn pad(slot: usize) -> Option<Pad> {
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: plain queries of the framework's controller list.
        let controllers = unsafe { GCController::controllers() };
        let gamepad = controllers.iter().filter_map(|c| unsafe { c.extendedGamepad() }).nth(slot)?;
        // SAFETY: as above, on a live gamepad profile.
        unsafe {
            let dpad = gamepad.dpad();
            let (left, right) = (gamepad.leftThumbstick(), gamepad.rightThumbstick());
            Some(Pad {
                a: gamepad.buttonA().isPressed(),
                b: gamepad.buttonB().isPressed(),
                x: gamepad.buttonX().isPressed(),
                y: gamepad.buttonY().isPressed(),
                menu: gamepad.buttonMenu().isPressed(),
                view: gamepad.buttonOptions().is_some_and(|b| b.isPressed()),
                up: dpad.up().isPressed(),
                down: dpad.down().isPressed(),
                left: dpad.left().isPressed(),
                right: dpad.right().isPressed(),
                left_shoulder: gamepad.leftShoulder().isPressed(),
                right_shoulder: gamepad.rightShoulder().isPressed(),
                left_stick: gamepad.leftThumbstickButton().is_some_and(|b| b.isPressed()),
                right_stick: gamepad.rightThumbstickButton().is_some_and(|b| b.isPressed()),
                left_trigger: gamepad.leftTrigger().value(),
                right_trigger: gamepad.rightTrigger().value(),
                left_x: left.xAxis().value(),
                left_y: left.yAxis().value(),
                right_x: right.xAxis().value(),
                right_y: right.yAxis().value(),
            })
        }
    })
}
