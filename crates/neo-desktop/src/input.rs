//! 键盘与鼠标输入。所有坐标使用虚拟桌面物理像素；
//! 所有动作在注入前检查取消标记，异常路径尽量释放已按下的键。

use crate::display;
use crate::error::{DesktopError, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 最近一次合成输入（SendInput）完成的墙钟毫秒；Neo 的迷你窗用它区分
/// 真人点击与 AI 注入的点击。
pub static SYNTHETIC_INPUT_AT: AtomicU64 = AtomicU64::new(0);

pub fn synthetic_input_at() -> u64 {
    SYNTHETIC_INPUT_AT.load(Ordering::Relaxed)
}

fn mark_synthetic() {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    SYNTHETIC_INPUT_AT.store(millis, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaretPosition {
    #[default]
    Idle,
    Start,
    End,
}

impl CaretPosition {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "idle" => Ok(Self::Idle),
            "start" => Ok(Self::Start),
            "end" => Ok(Self::End),
            other => Err(DesktopError::bad_args(format!(
                "caret 只能是 idle / start / end，收到 {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TypeOptions {
    pub clear: bool,
    pub press_enter: bool,
    pub caret: CaretPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDirection {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            other => Err(DesktopError::bad_args(format!(
                "direction 只能是 up / down / left / right，收到 {other:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    fn horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// 短文本逐字 Unicode 注入；长纯文本走剪贴板粘贴，避免扫描码队列丢键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypePlan {
    Paste,
    Unicode,
}

pub fn type_plan(text: &str) -> TypePlan {
    let plain = !text.chars().any(|c| matches!(c, '\n' | '\r' | '\t'));
    if plain && text.chars().count() >= 20 {
        TypePlan::Paste
    } else {
        TypePlan::Unicode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub vk: u16,
    pub shift: bool,
    pub extended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: Vec<KeyStroke>,
    pub key: KeyStroke,
}

/// 解析 `ctrl+shift+esc`、`win+r`、`alt+tab` 这类快捷键。
/// 只接受一个非修饰键；加号本身请写 `plus`。
pub fn parse_shortcut(chord: &str) -> Result<Chord> {
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return Err(DesktopError::bad_args("快捷键不能为空"));
    }
    let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(DesktopError::bad_args(
            "快捷键含空键名；加号键请写 plus，例如 ctrl+plus",
        ));
    }

    let single = parts.len() == 1;
    let mut modifiers = Vec::new();
    let mut keys = Vec::new();
    for part in &parts {
        if !single {
            if let Some(modifier) = modifier_key(part) {
                if !modifiers
                    .iter()
                    .any(|item: &KeyStroke| item.vk == modifier.vk)
                {
                    modifiers.push(modifier);
                }
                continue;
            }
        }
        keys.push(parse_key(part)?);
    }

    if keys.is_empty() {
        if single {
            keys.push(
                modifier_key(trimmed)
                    .ok_or_else(|| DesktopError::bad_args(format!("无法识别的按键 {trimmed:?}")))?,
            );
        } else {
            return Err(DesktopError::bad_args("快捷键缺少非修饰键"));
        }
    }
    if keys.len() != 1 {
        return Err(DesktopError::bad_args(
            "快捷键只能有一个非修饰键，例如 ctrl+c",
        ));
    }

    let mut key = keys.remove(0);
    if key.shift && !modifiers.iter().any(|item| item.vk == VK_SHIFT) {
        modifiers.push(KeyStroke {
            vk: VK_SHIFT,
            shift: false,
            extended: false,
        });
        key.shift = false;
    }
    Ok(Chord { modifiers, key })
}

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;

fn modifier_key(name: &str) -> Option<KeyStroke> {
    let (vk, extended) = match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" => (VK_CONTROL, false),
        "shift" => (VK_SHIFT, false),
        "alt" | "option" | "opt" => (VK_MENU, false),
        "win" | "windows" | "super" | "cmd" | "command" | "meta" => (VK_LWIN, true),
        _ => return None,
    };
    Some(KeyStroke {
        vk,
        shift: false,
        extended,
    })
}

fn key(vk: u16, extended: bool) -> KeyStroke {
    KeyStroke {
        vk,
        shift: false,
        extended,
    }
}

fn shifted(vk: u16) -> KeyStroke {
    KeyStroke {
        vk,
        shift: true,
        extended: false,
    }
}

fn parse_key(name: &str) -> Result<KeyStroke> {
    let lower = name.trim().to_ascii_lowercase();
    let named = match lower.as_str() {
        "backspace" | "back" => Some(key(0x08, false)),
        "tab" => Some(key(0x09, false)),
        "enter" | "return" => Some(key(0x0D, false)),
        "esc" | "escape" => Some(key(0x1B, false)),
        "space" | "spacebar" => Some(key(0x20, false)),
        "pageup" | "pgup" => Some(key(0x21, true)),
        "pagedown" | "pgdn" => Some(key(0x22, true)),
        "end" => Some(key(0x23, true)),
        "home" => Some(key(0x24, true)),
        "left" | "arrowleft" => Some(key(0x25, true)),
        "up" | "arrowup" => Some(key(0x26, true)),
        "right" | "arrowright" => Some(key(0x27, true)),
        "down" | "arrowdown" => Some(key(0x28, true)),
        "insert" | "ins" => Some(key(0x2D, true)),
        "delete" | "del" => Some(key(0x2E, true)),
        "printscreen" | "prtsc" | "snapshot" => Some(key(0x2C, true)),
        "pause" => Some(key(0x13, false)),
        "capslock" => Some(key(0x14, false)),
        "numlock" => Some(key(0x90, true)),
        "scrolllock" => Some(key(0x91, false)),
        "apps" | "menu" => Some(key(0x5D, true)),
        "plus" => Some(key(0xBB, false)),
        "minus" | "dash" | "hyphen" => Some(key(0xBD, false)),
        "comma" => Some(key(0xBC, false)),
        "period" | "dot" => Some(key(0xBE, false)),
        "slash" => Some(key(0xBF, false)),
        "semicolon" => Some(key(0xBA, false)),
        "quote" | "apostrophe" => Some(key(0xDE, false)),
        "bracketleft" | "lbracket" => Some(key(0xDB, false)),
        "bracketright" | "rbracket" => Some(key(0xDD, false)),
        "backslash" => Some(key(0xDC, false)),
        "grave" | "backtick" => Some(key(0xC0, false)),
        "equal" | "equals" => Some(key(0xBB, false)),
        _ => None,
    };
    if let Some(stroke) = named {
        return Ok(stroke);
    }

    if let Some(rest) = lower.strip_prefix('f') {
        if let Ok(number) = rest.parse::<u16>() {
            if (1..=24).contains(&number) {
                return Ok(key(0x6F + number, false));
            }
        }
    }

    let mut chars = lower.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        if let Some(stroke) = char_key(ch) {
            return Ok(stroke);
        }
    }

    Err(DesktopError::bad_args(format!(
        "无法识别的按键 {name:?}；常用键：ctrl / shift / alt / win、a-z、0-9、f1-f24、\
         enter / esc / tab / space / backspace / delete / home / end / pageup / pagedown / \
         方向键、plus / minus / comma / period / slash / semicolon / quote / bracketleft / \
         bracketright / backslash / grave"
    )))
}

fn char_key(ch: char) -> Option<KeyStroke> {
    Some(match ch {
        'a'..='z' => key(0x41 + (ch as u16 - 'a' as u16), false),
        '0'..='9' => key(0x30 + (ch as u16 - '0' as u16), false),
        '!' => shifted(0x31),
        '@' => shifted(0x32),
        '#' => shifted(0x33),
        '$' => shifted(0x34),
        '%' => shifted(0x35),
        '^' => shifted(0x36),
        '&' => shifted(0x37),
        '*' => shifted(0x38),
        '(' => shifted(0x39),
        ')' => shifted(0x30),
        '_' => shifted(0xBD),
        '+' => shifted(0xBB),
        '~' => shifted(0xC0),
        '{' => shifted(0xDB),
        '}' => shifted(0xDD),
        '|' => shifted(0xDC),
        ':' => shifted(0xBA),
        '"' => shifted(0xDE),
        '<' => shifted(0xBC),
        '>' => shifted(0xBE),
        '?' => shifted(0xBF),
        ',' => key(0xBC, false),
        '.' => key(0xBE, false),
        '/' => key(0xBF, false),
        ';' => key(0xBA, false),
        '\'' => key(0xDE, false),
        '[' => key(0xDB, false),
        ']' => key(0xDD, false),
        '\\' => key(0xDC, false),
        '-' => key(0xBD, false),
        '=' => key(0xBB, false),
        '`' => key(0xC0, false),
        _ => return None,
    })
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
        KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK,
        MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    };

    fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn mouse_move_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn mouse_wheel_input(flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }
        mark_synthetic();
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        mark_synthetic();
        if sent as usize != inputs.len() {
            return Err(DesktopError::not_allowed(format!(
                "输入事件没有全部送达（{sent} / {}）。目标窗口可能以管理员身份运行，\
                 而 Neo 不是；请用管理员身份启动 Neo 或换一个目标",
                inputs.len()
            )));
        }
        Ok(())
    }

    fn scan_code(vk: u16) -> u16 {
        unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) as u16 }
    }

    fn stroke_inputs(stroke: KeyStroke, down: bool) -> Vec<INPUT> {
        let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
        if stroke.extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        vec![key_input(stroke.vk, scan_code(stroke.vk), flags)]
    }

    fn send_chord(chord: &Chord) -> Result<()> {
        let mut inputs = Vec::with_capacity((chord.modifiers.len() * 2 + 1) * 2);
        for modifier in &chord.modifiers {
            inputs.extend(stroke_inputs(*modifier, true));
        }
        inputs.extend(stroke_inputs(chord.key, true));
        inputs.extend(stroke_inputs(chord.key, false));
        for modifier in chord.modifiers.iter().rev() {
            inputs.extend(stroke_inputs(*modifier, false));
        }
        send(&inputs)
    }

    fn send_unicode(unit: u16) -> Result<()> {
        send(&[
            key_input(0, unit, KEYEVENTF_UNICODE),
            key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ])
    }

    fn send_key(vk: u16, extended: bool) -> Result<()> {
        send(&stroke_inputs(
            KeyStroke {
                vk,
                shift: false,
                extended,
            },
            true,
        ))?;
        send(&stroke_inputs(
            KeyStroke {
                vk,
                shift: false,
                extended,
            },
            false,
        ))
    }

    fn paste_text(text: &str, cancelled: &dyn Fn() -> bool) -> Result<()> {
        if cancelled() {
            return Err(DesktopError::not_allowed("工具调用已取消"));
        }
        let previous = crate::clipboard::get_text().ok().flatten();
        crate::clipboard::set_text(text)?;
        std::thread::sleep(Duration::from_millis(50));
        send_chord(&parse_shortcut("ctrl+v")?)?;
        std::thread::sleep(Duration::from_millis(80));
        if let Some(previous) = previous {
            if crate::clipboard::get_text().ok().flatten().as_deref() == Some(text) {
                let _ = crate::clipboard::set_text(&previous);
            }
        }
        Ok(())
    }

    fn type_unicode(text: &str, cancelled: &dyn Fn() -> bool) -> Result<()> {
        for ch in text.chars() {
            if cancelled() {
                return Err(DesktopError::not_allowed("工具调用已取消"));
            }
            match ch {
                '\r' => continue,
                '\n' => send_key(0x0D, false)?,
                '\t' => send_key(0x09, false)?,
                _ => {
                    let mut units = [0u16; 2];
                    for unit in ch.encode_utf16(&mut units) {
                        send_unicode(*unit)?;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(35));
        }
        Ok(())
    }

    pub fn type_text(text: &str, options: TypeOptions, cancelled: impl Fn() -> bool) -> Result<()> {
        display::ensure_dpi_aware();
        if options.clear {
            if cancelled() {
                return Err(DesktopError::not_allowed("工具调用已取消"));
            }
            send_chord(&parse_shortcut("ctrl+a")?)?;
            std::thread::sleep(Duration::from_millis(50));
            send_chord(&parse_shortcut("backspace")?)?;
            std::thread::sleep(Duration::from_millis(50));
        }
        match options.caret {
            CaretPosition::Idle => {}
            CaretPosition::Start => {
                send_key(0x24, true)?;
                std::thread::sleep(Duration::from_millis(30));
            }
            CaretPosition::End => {
                send_key(0x23, true)?;
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        if !text.is_empty() {
            let pasted = if type_plan(text) == TypePlan::Paste {
                paste_text(text, &cancelled).is_ok()
            } else {
                false
            };
            if !pasted {
                type_unicode(text, &cancelled)?;
            }
        }
        if options.press_enter {
            send_key(0x0D, false)?;
        }
        Ok(())
    }

    pub fn shortcut(chord: &str, repeat: u32, cancelled: impl Fn() -> bool) -> Result<()> {
        let chord = parse_shortcut(chord)?;
        let repeat = repeat.clamp(1, 10);
        for _ in 0..repeat {
            if cancelled() {
                return Err(DesktopError::not_allowed("工具调用已取消"));
            }
            send_chord(&chord)?;
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    pub fn scroll(
        direction: ScrollDirection,
        amount: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        let amount = amount.clamp(1, 20);
        for _ in 0..amount {
            if cancelled() {
                return Err(DesktopError::not_allowed("工具调用已取消"));
            }
            if direction.horizontal() {
                let delta = if direction == ScrollDirection::Left {
                    120u32
                } else {
                    (-120i32) as u32
                };
                let inputs = [
                    key_input(VK_SHIFT, scan_code(VK_SHIFT), 0),
                    mouse_wheel_input(MOUSEEVENTF_WHEEL, delta),
                    key_input(VK_SHIFT, scan_code(VK_SHIFT), KEYEVENTF_KEYUP),
                ];
                send(&inputs)?;
            } else {
                let delta = if direction == ScrollDirection::Up {
                    120u32
                } else {
                    (-120i32) as u32
                };
                send(&[mouse_wheel_input(MOUSEEVENTF_WHEEL, delta)])?;
            }
            std::thread::sleep(Duration::from_millis(35));
        }
        Ok(())
    }

    pub fn move_mouse(
        x: i32,
        y: i32,
        duration_ms: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

        display::ensure_dpi_aware();
        let screen = display::virtual_screen();
        let target_x = display::normalize_axis(x, screen.x, screen.width)?;
        let target_y = display::normalize_axis(y, screen.y, screen.height)?;
        let mut start = POINT { x, y };
        unsafe {
            let _ = GetCursorPos(&mut start);
        }
        if duration_ms == 0 {
            return send(&[mouse_move_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                target_x,
                target_y,
            )]);
        }

        let steps = duration_ms.div_ceil(10).clamp(1, 200);
        let interval = Duration::from_millis((duration_ms / steps).max(1));
        let (sx, sy) = (i64::from(start.x), i64::from(start.y));
        let (tx, ty) = (i64::from(x), i64::from(y));
        for step in 1..=steps {
            if cancelled() {
                return Err(DesktopError::not_allowed("工具调用已取消"));
            }
            let ratio = step as f64 / steps as f64;
            let px = (sx + ((tx - sx) as f64 * ratio).round() as i64) as i32;
            let py = (sy + ((ty - sy) as f64 * ratio).round() as i64) as i32;
            let nx = display::normalize_axis(px, screen.x, screen.width)?;
            let ny = display::normalize_axis(py, screen.y, screen.height)?;
            send(&[mouse_move_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                nx,
                ny,
            )])?;
            if step < steps {
                std::thread::sleep(interval);
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn type_text(
        _text: &str,
        _options: TypeOptions,
        _cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        Err(DesktopError::unsupported("键盘输入只在 Windows 上可用"))
    }

    pub fn shortcut(_chord: &str, _repeat: u32, _cancelled: impl Fn() -> bool) -> Result<()> {
        Err(DesktopError::unsupported("键盘输入只在 Windows 上可用"))
    }

    pub fn scroll(
        _direction: ScrollDirection,
        _amount: u32,
        _cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        Err(DesktopError::unsupported("鼠标滚动只在 Windows 上可用"))
    }

    pub fn move_mouse(
        _x: i32,
        _y: i32,
        _duration_ms: u64,
        _cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        Err(DesktopError::unsupported("鼠标移动只在 Windows 上可用"))
    }
}

pub use imp::{move_mouse, scroll, shortcut, type_text};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_chords() {
        let chord = parse_shortcut("ctrl+shift+esc").unwrap();
        assert_eq!(
            chord.modifiers.iter().map(|k| k.vk).collect::<Vec<_>>(),
            vec![VK_CONTROL, VK_SHIFT]
        );
        assert_eq!(chord.key.vk, 0x1B);

        let chord = parse_shortcut("win+r").unwrap();
        assert_eq!(chord.modifiers[0].vk, VK_LWIN);
        assert_eq!(chord.key.vk, 0x52);

        let chord = parse_shortcut("alt+tab").unwrap();
        assert_eq!(chord.modifiers[0].vk, VK_MENU);
        assert_eq!(chord.key.vk, 0x09);

        let chord = parse_shortcut("ctrl+plus").unwrap();
        assert_eq!(chord.key.vk, 0xBB);
        assert!(!chord.key.shift);
    }

    #[test]
    fn parses_shifted_characters() {
        let chord = parse_shortcut("ctrl+?").unwrap();
        assert!(chord.modifiers.iter().any(|k| k.vk == VK_SHIFT));
        assert_eq!(chord.key.vk, 0xBF);
    }

    #[test]
    fn rejects_bad_chords() {
        assert!(parse_shortcut("").is_err());
        assert!(parse_shortcut("ctrl+").is_err());
        assert!(parse_shortcut("ctrl+a+b").is_err());
        assert!(parse_shortcut("ctrl+nope").is_err());
    }

    #[test]
    fn type_plan_uses_paste_only_for_long_plain_text() {
        assert_eq!(type_plan("short"), TypePlan::Unicode);
        assert_eq!(type_plan("this is a long plain text"), TypePlan::Paste);
        assert_eq!(
            type_plan("line one\nline two is long enough"),
            TypePlan::Unicode
        );
        assert_eq!(type_plan("tab\there and long enough"), TypePlan::Unicode);
    }

    #[test]
    fn caret_and_scroll_parse() {
        assert_eq!(CaretPosition::parse("start").unwrap(), CaretPosition::Start);
        assert!(CaretPosition::parse("middle").is_err());
        assert_eq!(
            ScrollDirection::parse("left").unwrap(),
            ScrollDirection::Left
        );
        assert!(ScrollDirection::parse("forward").is_err());
    }
}
