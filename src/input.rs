//! Synthesising key presses for button actions.
//!
//! Media keys are sent as virtual keys: Windows routes them to whatever is
//! playing, wherever the focus is. But Windows drops synthesised input
//! while a window running as administrator has the focus (blu-ghub does not
//! run elevated), and while the PC is locked or showing a UAC prompt: the
//! key never enters the input stream, and SendInput does not always say so.
//! A real keyboard's media keys work there; these do not.

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP,
    VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};

/// What a headset button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonAction {
    /// Leave the button to the headset's own firmware. Only possible while
    /// no button is taken over: the headset hands over all three or none.
    #[default]
    Default,
    /// Nothing.
    #[serde(alias = "disabled", alias = "none")]
    Off,
    PlayPause,
    Stop,
    NextTrack,
    PreviousTrack,
    /// Mute or unmute the PC's sound output.
    Mute,
    VolumeUp,
    VolumeDown,
}

impl ButtonAction {
    fn vk(self) -> Option<VIRTUAL_KEY> {
        Some(match self {
            ButtonAction::PlayPause => VK_MEDIA_PLAY_PAUSE,
            ButtonAction::Stop => VK_MEDIA_STOP,
            ButtonAction::NextTrack => VK_MEDIA_NEXT_TRACK,
            ButtonAction::PreviousTrack => VK_MEDIA_PREV_TRACK,
            ButtonAction::Mute => VK_VOLUME_MUTE,
            ButtonAction::VolumeUp => VK_VOLUME_UP,
            ButtonAction::VolumeDown => VK_VOLUME_DOWN,
            ButtonAction::Default | ButtonAction::Off => return None,
        })
    }

    /// Carry the action out.
    pub fn run(self) -> std::io::Result<()> {
        match self.vk() {
            Some(vk) => tap(vk),
            None => Ok(()),
        }
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    // Media keys are extended keys.
    let flags = if up { KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP } else { KEYEVENTF_EXTENDEDKEY };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Press and release `vk` in one call, so nothing can come between the two.
fn tap(vk: VIRTUAL_KEY) -> std::io::Result<()> {
    let inputs = [key(vk, false), key(vk, true)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ButtonAction; 9] = [
        ButtonAction::Default,
        ButtonAction::Off,
        ButtonAction::PlayPause,
        ButtonAction::Stop,
        ButtonAction::NextTrack,
        ButtonAction::PreviousTrack,
        ButtonAction::Mute,
        ButtonAction::VolumeUp,
        ButtonAction::VolumeDown,
    ];

    #[test]
    fn every_action_but_default_and_off_sends_a_key() {
        for a in ALL {
            let quiet = matches!(a, ButtonAction::Default | ButtonAction::Off);
            assert_eq!(a.vk().is_none(), quiet, "{a:?}");
        }
    }

    #[test]
    fn media_keys_go_down_and_up_as_extended_keys() {
        let flags = |i: INPUT| unsafe { i.Anonymous.ki.dwFlags };
        assert_eq!(flags(key(VK_MEDIA_PLAY_PAUSE, false)), KEYEVENTF_EXTENDEDKEY);
        assert_eq!(flags(key(VK_MEDIA_PLAY_PAUSE, true)), KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
    }
}
