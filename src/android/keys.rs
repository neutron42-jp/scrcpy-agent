//! Android keycode and modifier lookups for agent-friendly key names.

pub fn keycode(name: &str) -> Option<u32> {
    let upper = name.trim().to_ascii_uppercase();
    let upper = upper.strip_prefix("KEYCODE_").unwrap_or(&upper);
    let upper = upper.strip_prefix("KEY_").unwrap_or(upper);

    if upper.chars().count() == 1 {
        let ch = upper.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return Some(29 + (ch as u32 - 'A' as u32));
        }
        if ch.is_ascii_digit() {
            return Some(7 + (ch as u32 - '0' as u32));
        }
    }
    if let Ok(raw) = upper.parse::<u32>() {
        return Some(raw);
    }

    let code = match upper {
        "HOME" => 3,
        "BACK" => 4,
        "CALL" => 5,
        "ENDCALL" => 6,
        "STAR" => 17,
        "POUND" => 18,
        "DPAD_UP" | "UP" | "ARROW_UP" => 19,
        "DPAD_DOWN" | "DOWN" | "ARROW_DOWN" => 20,
        "DPAD_LEFT" | "LEFT" | "ARROW_LEFT" => 21,
        "DPAD_RIGHT" | "RIGHT" | "ARROW_RIGHT" => 22,
        "DPAD_CENTER" | "CENTER" => 23,
        "VOLUME_UP" => 24,
        "VOLUME_DOWN" => 25,
        "VOLUME_MUTE" | "MUTE" => 164,
        "POWER" => 26,
        "CAMERA" => 27,
        "CLEAR" => 28,
        "COMMA" => 55,
        "PERIOD" => 56,
        "ALT_LEFT" | "ALT" => 57,
        "ALT_RIGHT" => 58,
        "SHIFT_LEFT" | "SHIFT" => 59,
        "SHIFT_RIGHT" => 60,
        "TAB" => 61,
        "SPACE" | " " => 62,
        "SYM" => 63,
        "ENTER" | "RETURN" => 66,
        "DEL" | "DELETE" | "BACKSPACE" => 67,
        "GRAVE" => 68,
        "MINUS" => 69,
        "EQUALS" => 70,
        "LEFT_BRACKET" => 71,
        "RIGHT_BRACKET" => 72,
        "BACKSLASH" => 73,
        "SEMICOLON" => 74,
        "APOSTROPHE" => 75,
        "SLASH" => 76,
        "AT" => 77,
        "NUM" => 78,
        "HEADSETHOOK" => 79,
        "FOCUS" => 80,
        "PLUS" => 81,
        "MENU" => 82,
        "NOTIFICATION" => 83,
        "SEARCH" => 84,
        "MEDIA_PLAY_PAUSE" => 85,
        "MEDIA_STOP" => 86,
        "MEDIA_NEXT" => 87,
        "MEDIA_PREVIOUS" => 88,
        "MEDIA_REWIND" => 89,
        "MEDIA_FAST_FORWARD" => 90,
        "PAGE_UP" => 92,
        "PAGE_DOWN" => 93,
        "ESCAPE" | "ESC" => 111,
        "FORWARD_DEL" => 112,
        "CTRL_LEFT" | "CTRL" | "CONTROL" => 113,
        "CTRL_RIGHT" => 114,
        "CAPS_LOCK" => 115,
        "META_LEFT" | "META" | "SUPER" => 117,
        "META_RIGHT" => 118,
        "MOVE_HOME" => 122,
        "MOVE_END" => 123,
        "INSERT" => 124,
        "MEDIA_PLAY" => 126,
        "MEDIA_PAUSE" => 127,
        "APP_SWITCH" | "RECENTS" => 187,
        "ASSIST" | "SEARCH_ASSISTANT" => 219,
        "BRIGHTNESS_DOWN" => 220,
        "BRIGHTNESS_UP" => 221,
        "SLEEP" => 223,
        "WAKEUP" | "WAKE" => 224,
        "PASTE" => 279,
        _ => return None,
    };
    Some(code)
}

pub fn meta_state(modifiers: &[String]) -> u32 {
    let mut meta = 0u32;
    for modifier in modifiers {
        match modifier.trim().to_ascii_lowercase().as_str() {
            "shift" => meta |= 0x1,
            "alt" => meta |= 0x2,
            "ctrl" | "control" => meta |= 0x1000,
            "meta" | "super" | "win" => meta |= 0x10000,
            "caps" => meta |= 0x100000,
            "fn" | "function" => meta |= 0x8,
            other => log::warn!("ignoring unknown modifier {other}"),
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_named_and_numeric_keys() {
        assert_eq!(keycode("ENTER"), Some(66));
        assert_eq!(keycode("enter"), Some(66));
        assert_eq!(keycode("KEYCODE_BACK"), Some(4));
        assert_eq!(keycode("a"), Some(29));
        assert_eq!(keycode("7"), Some(14));
        assert_eq!(keycode("66"), Some(66));
        assert_eq!(keycode("nope"), None);
    }

    #[test]
    fn combines_modifiers() {
        assert_eq!(meta_state(&["shift".into(), "ctrl".into()]), 0x1001);
    }
}
