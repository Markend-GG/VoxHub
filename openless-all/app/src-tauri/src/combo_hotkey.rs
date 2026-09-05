//! 录音快捷键的自定义组合键监听器。
//!
//! 与 `hotkey.rs`（modifier-only 听写热键）平行——当用户选择自定义组合键
//! （如 `Cmd+Shift+D`）时，用 `global-hotkey` crate 注册。
//!
//! 与 `qa_hotkey.rs` 的关键区别：**同时产出 Pressed 和 Released 边沿事件**，
//! 以支持 Hold（按住说话）模式。`global-hotkey` crate 的 `HotKeyState::Released`
//! 在 macOS (Carbon) 和 Windows 上均可用于检测松开。
//!
//! 通过 `global_hotkey_runtime` 与 QA 快捷键共享进程级 manager / event receiver。

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use parking_lot::Mutex;

use crate::global_hotkey_runtime::{GlobalHotkeyRuntime, RegisteredHotkey};
use crate::shortcut_binding::{parse_global_hotkey, ShortcutBindingError};
use crate::types::ShortcutBinding;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboHotkeyEvent {
    /// 用户按下了配置的组合键。
    Pressed { at: Instant },
    /// 用户松开了配置的组合键（用于 Hold 模式结束录音）。
    Released { at: Instant },
}

#[derive(Debug, thiserror::Error)]
pub enum ComboHotkeyError {
    #[error("不支持的修饰键: {0}")]
    UnsupportedModifier(String),
    #[error("不支持的主键: {0}")]
    UnsupportedKey(String),
    #[error("注册全局快捷键失败: {0}")]
    RegisterFailed(String),
    #[error("初始化全局快捷键管理器失败: {0}")]
    ManagerInitFailed(String),
}

/// 自定义组合键全局快捷键监听器。`Drop` 时反注册。
///
/// 内部用 `global-hotkey` crate；事件转发线程持有一个共享的 `Sender`。
/// 与 `QaHotkeyMonitor` 的区别：转发 Pressed **和** Released 事件。
pub struct ComboHotkeyMonitor {
    inner: Arc<Inner>,
}

struct Inner {
    registered: Mutex<Option<RegisteredHotkey>>,
    tx: Sender<ComboHotkeyEvent>,
}

// global-hotkey 0.6 的 GlobalHotKeyManager 在 Windows 内部持有 HHOOK / window
// handle 等 `*mut c_void`，crate 没标 Send/Sync。与 qa_hotkey.rs 同理。
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

pub enum ActionHotkeyMonitor {
    Registered(ComboHotkeyMonitor),
    #[cfg(target_os = "windows")]
    Passthrough(PassthroughComboHotkeyMonitor),
}

impl ActionHotkeyMonitor {
    pub fn start_registered(
        binding: ShortcutBinding,
        tx: Sender<ComboHotkeyEvent>,
    ) -> Result<Self, ComboHotkeyError> {
        ComboHotkeyMonitor::start(binding, tx).map(Self::Registered)
    }

    pub fn start_passthrough(
        binding: ShortcutBinding,
        tx: Sender<ComboHotkeyEvent>,
    ) -> Result<Self, ComboHotkeyError> {
        #[cfg(target_os = "windows")]
        {
            PassthroughComboHotkeyMonitor::start(binding, tx).map(Self::Passthrough)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Self::start_registered(binding, tx)
        }
    }

    pub fn update_registered(&mut self, binding: ShortcutBinding) -> Result<(), ComboHotkeyError> {
        match self {
            Self::Registered(monitor) => monitor.update_binding(binding),
            #[cfg(target_os = "windows")]
            Self::Passthrough(_) => {
                *self = Self::start_registered(binding, self.sender())?;
                Ok(())
            }
        }
    }

    pub fn update_passthrough(&mut self, binding: ShortcutBinding) -> Result<(), ComboHotkeyError> {
        match self {
            Self::Registered(_) => {
                *self = Self::start_passthrough(binding, self.sender())?;
                Ok(())
            }
            #[cfg(target_os = "windows")]
            Self::Passthrough(monitor) => monitor.update_binding(binding),
        }
    }

    fn sender(&self) -> Sender<ComboHotkeyEvent> {
        match self {
            Self::Registered(monitor) => monitor.sender(),
            #[cfg(target_os = "windows")]
            Self::Passthrough(monitor) => monitor.sender(),
        }
    }
}

impl ComboHotkeyMonitor {
    /// 启动监听并注册一个组合键。`tx` 在每次按下/松开边沿收到事件。
    ///
    /// **注意**：`global-hotkey` crate 在 macOS 要求 manager 在主线程构造。
    /// 调用方需要确保从主线程触发。
    pub fn start(
        binding: ShortcutBinding,
        tx: Sender<ComboHotkeyEvent>,
    ) -> Result<Self, ComboHotkeyError> {
        let runtime = GlobalHotkeyRuntime::shared()
            .map_err(|e| ComboHotkeyError::ManagerInitFailed(e.to_string()))?;

        let hotkey = parse_binding(&binding)?;
        let (registered, rx) = runtime
            .register(hotkey)
            .map_err(|e| ComboHotkeyError::RegisterFailed(e.to_string()))?;

        // runtime 已按 hotkey id 分发；这里保留 id 检查作为防线，
        // 避免未来误接回进程级事件流后串到其他快捷键。
        let hotkey_id = registered.hotkey().id();
        let tx_for_thread = tx.clone();
        std::thread::Builder::new()
            .name("openless-combo-hotkey-forward".into())
            .spawn(move || forward_loop(hotkey_id, rx, tx_for_thread))
            .map_err(|e| ComboHotkeyError::RegisterFailed(format!("spawn forward thread: {e}")))?;

        Ok(Self {
            inner: Arc::new(Inner {
                registered: Mutex::new(Some(registered)),
                tx,
            }),
        })
    }

    /// 替换当前注册的组合键（用户在设置里改了组合键时）。
    pub fn update_binding(&self, binding: ShortcutBinding) -> Result<(), ComboHotkeyError> {
        let next = parse_binding(&binding)?;
        let mut current = self.inner.registered.lock();
        if let Some(prev) = current.as_ref() {
            if prev.hotkey() == next {
                return Ok(());
            }
        }
        let runtime = GlobalHotkeyRuntime::shared()
            .map_err(|e| ComboHotkeyError::ManagerInitFailed(e.to_string()))?;
        let (registered, rx) = runtime
            .register(next)
            .map_err(|e| ComboHotkeyError::RegisterFailed(e.to_string()))?;
        let hotkey_id = registered.hotkey().id();
        std::thread::Builder::new()
            .name("openless-combo-hotkey-forward".into())
            .spawn({
                let tx = self.inner.tx.clone();
                move || forward_loop(hotkey_id, rx, tx)
            })
            .map_err(|e| ComboHotkeyError::RegisterFailed(format!("spawn forward thread: {e}")))?;
        *current = Some(registered);
        Ok(())
    }

    fn sender(&self) -> Sender<ComboHotkeyEvent> {
        self.inner.tx.clone()
    }
}

impl Drop for ComboHotkeyMonitor {
    fn drop(&mut self) {
        self.inner.registered.lock().take();
    }
}

fn forward_loop(hotkey_id: u32, rx: Receiver<GlobalHotKeyEvent>, tx: Sender<ComboHotkeyEvent>) {
    while let Ok(event) = rx.recv() {
        if event.id() != hotkey_id {
            continue;
        }
        let at = Instant::now();
        let combo_event = match event.state() {
            HotKeyState::Pressed => ComboHotkeyEvent::Pressed { at },
            HotKeyState::Released => ComboHotkeyEvent::Released { at },
        };
        if let Err(e) = tx.send(combo_event) {
            log::warn!("[combo-hotkey] 事件投递失败: {e}");
            break;
        }
    }
    log::info!("[combo-hotkey] 转发线程退出");
}

/// 测试一个组合键是否可以注册（不实际注册，仅验证格式）。
pub fn validate_binding(binding: &ShortcutBinding) -> Result<(), ComboHotkeyError> {
    parse_binding(binding)?;
    Ok(())
}

fn parse_binding(
    binding: &ShortcutBinding,
) -> Result<global_hotkey::hotkey::HotKey, ComboHotkeyError> {
    parse_global_hotkey(binding).map_err(|e| match e {
        ShortcutBindingError::UnsupportedModifier(m) => ComboHotkeyError::UnsupportedModifier(m),
        ShortcutBindingError::UnsupportedKey(k) => ComboHotkeyError::UnsupportedKey(k),
    })
}

#[cfg(target_os = "windows")]
pub struct PassthroughComboHotkeyMonitor {
    inner: Arc<passthrough_windows::Inner>,
}

#[cfg(target_os = "windows")]
impl PassthroughComboHotkeyMonitor {
    pub fn start(
        binding: ShortcutBinding,
        tx: Sender<ComboHotkeyEvent>,
    ) -> Result<Self, ComboHotkeyError> {
        passthrough_windows::start(binding, tx).map(|inner| Self { inner })
    }

    pub fn update_binding(&self, binding: ShortcutBinding) -> Result<(), ComboHotkeyError> {
        passthrough_windows::update_binding(&self.inner, binding)
    }

    fn sender(&self) -> Sender<ComboHotkeyEvent> {
        self.inner.tx.clone()
    }
}

#[cfg(target_os = "windows")]
impl Drop for PassthroughComboHotkeyMonitor {
    fn drop(&mut self) {
        passthrough_windows::shutdown(&self.inner);
    }
}

#[cfg(target_os = "windows")]
mod passthrough_windows {
    use super::*;
    use global_hotkey::hotkey::{Code, Modifiers};
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG,
        WH_KEYBOARD_LL, WM_QUIT,
    };

    const WM_KEYDOWN: usize = 0x0100;
    const WM_KEYUP: usize = 0x0101;
    const WM_SYSKEYDOWN: usize = 0x0104;
    const WM_SYSKEYUP: usize = 0x0105;

    const VK_BACK: u32 = 0x08;
    const VK_TAB: u32 = 0x09;
    const VK_RETURN: u32 = 0x0D;
    const VK_SHIFT: u32 = 0x10;
    const VK_CONTROL: u32 = 0x11;
    const VK_MENU: u32 = 0x12;
    const VK_ESCAPE: u32 = 0x1B;
    const VK_SPACE: u32 = 0x20;
    const VK_PRIOR: u32 = 0x21;
    const VK_NEXT: u32 = 0x22;
    const VK_END: u32 = 0x23;
    const VK_HOME: u32 = 0x24;
    const VK_LEFT: u32 = 0x25;
    const VK_UP: u32 = 0x26;
    const VK_RIGHT: u32 = 0x27;
    const VK_DOWN: u32 = 0x28;
    const VK_DELETE: u32 = 0x2E;
    const VK_LSHIFT: u32 = 0xA0;
    const VK_RSHIFT: u32 = 0xA1;
    const VK_LCONTROL: u32 = 0xA2;
    const VK_RCONTROL: u32 = 0xA3;
    const VK_LMENU: u32 = 0xA4;
    const VK_RMENU: u32 = 0xA5;
    const VK_LWIN: u32 = 0x5B;
    const VK_RWIN: u32 = 0x5C;

    static HOOK_CONTEXT: AtomicPtr<CallbackContext> = AtomicPtr::new(std::ptr::null_mut());

    pub struct Inner {
        binding: parking_lot::RwLock<BindingMatcher>,
        pub tx: Sender<ComboHotkeyEvent>,
        thread_id: parking_lot::Mutex<Option<u32>>,
        ctrl_held: AtomicBool,
        alt_held: AtomicBool,
        shift_held: AtomicBool,
        super_held: AtomicBool,
        primary_held: AtomicBool,
        matched_held: AtomicBool,
        installed: AtomicBool,
    }

    unsafe impl Send for Inner {}
    unsafe impl Sync for Inner {}

    struct CallbackContext {
        inner: Arc<Inner>,
        hook: std::sync::Mutex<Option<HHOOK>>,
    }

    unsafe impl Send for CallbackContext {}
    unsafe impl Sync for CallbackContext {}

    pub fn start(
        binding: ShortcutBinding,
        tx: Sender<ComboHotkeyEvent>,
    ) -> Result<Arc<Inner>, ComboHotkeyError> {
        let matcher = BindingMatcher::from_binding(&binding)?;
        let inner = Arc::new(Inner {
            binding: parking_lot::RwLock::new(matcher),
            tx,
            thread_id: parking_lot::Mutex::new(None),
            ctrl_held: AtomicBool::new(false),
            alt_held: AtomicBool::new(false),
            shift_held: AtomicBool::new(false),
            super_held: AtomicBool::new(false),
            primary_held: AtomicBool::new(false),
            matched_held: AtomicBool::new(false),
            installed: AtomicBool::new(false),
        });
        let (status_tx, status_rx) = std::sync::mpsc::sync_channel(1);
        let inner_for_thread = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("openless-passthrough-combo-hotkey-win-ll-hook".into())
            .spawn(move || run_listen_loop(inner_for_thread, status_tx))
            .map_err(|e| ComboHotkeyError::RegisterFailed(format!("spawn hook thread: {e}")))?;

        let thread_id = status_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| {
                ComboHotkeyError::RegisterFailed(
                    "Windows pass-through hotkey hook startup timeout".into(),
                )
            })??;
        *inner.thread_id.lock() = Some(thread_id);
        inner.installed.store(true, Ordering::SeqCst);
        Ok(inner)
    }

    pub fn update_binding(
        inner: &Arc<Inner>,
        binding: ShortcutBinding,
    ) -> Result<(), ComboHotkeyError> {
        let matcher = BindingMatcher::from_binding(&binding)?;
        *inner.binding.write() = matcher;
        inner.primary_held.store(false, Ordering::SeqCst);
        inner.matched_held.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn shutdown(inner: &Arc<Inner>) {
        let Some(thread_id) = *inner.thread_id.lock() else {
            return;
        };
        unsafe {
            if let Err(err) = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) {
                log::warn!("[combo-hotkey] pass-through Windows hook shutdown failed: {err}");
            }
        }
    }

    fn run_listen_loop(
        inner: Arc<Inner>,
        status_tx: std::sync::mpsc::SyncSender<Result<u32, ComboHotkeyError>>,
    ) {
        let thread_id = unsafe { GetCurrentThreadId() };
        let context = Box::into_raw(Box::new(CallbackContext {
            inner,
            hook: std::sync::Mutex::new(None),
        }));
        HOOK_CONTEXT.store(context, Ordering::SeqCst);

        unsafe {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), None, 0);
            match hook {
                Ok(hook) => {
                    *(*context).hook.lock().unwrap() = Some(hook);
                    let _ = status_tx.send(Ok(thread_id));
                    log::info!("[combo-hotkey] Windows pass-through hook installed");
                }
                Err(err) => {
                    HOOK_CONTEXT.store(std::ptr::null_mut(), Ordering::SeqCst);
                    let _ = Box::from_raw(context);
                    let _ = status_tx.send(Err(ComboHotkeyError::RegisterFailed(format!(
                        "Windows pass-through hotkey hook install failed: {err}"
                    ))));
                    return;
                }
            }

            let mut message = MSG::default();
            loop {
                let result = GetMessageW(&mut message, None, 0, 0).0;
                if result <= 0 {
                    break;
                }
                let _ = TranslateMessage(&message);
                let _ = DispatchMessageW(&message);
            }

            if let Some(hook) = (*context).hook.lock().unwrap().take() {
                let _ = UnhookWindowsHookEx(hook);
            }
            (&(*context).inner).installed.store(false, Ordering::SeqCst);
            HOOK_CONTEXT.store(std::ptr::null_mut(), Ordering::SeqCst);
            let _ = Box::from_raw(context);
            log::info!("[combo-hotkey] Windows pass-through hook exited");
        }
    }

    unsafe extern "system" fn low_level_keyboard_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code == HC_ACTION as i32 && lparam.0 != 0 {
            if let Some(ctx) = callback_context() {
                let keyboard = *(lparam.0 as *const KBDLLHOOKSTRUCT);
                dispatch_keyboard_event(&ctx.inner, keyboard.vkCode, wparam.0);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    unsafe fn callback_context<'a>() -> Option<&'a CallbackContext> {
        let ptr = HOOK_CONTEXT.load(Ordering::SeqCst);
        if ptr.is_null() {
            None
        } else {
            Some(&*ptr)
        }
    }

    fn dispatch_keyboard_event(inner: &Arc<Inner>, vk_code: u32, message: usize) {
        let is_down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let is_up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
        if !is_down && !is_up {
            return;
        }

        update_modifier_state(inner, vk_code, is_down, is_up);

        let matcher = *inner.binding.read();
        if vk_code != matcher.vk_code {
            return;
        }

        if is_down {
            let was_held = inner.primary_held.swap(true, Ordering::SeqCst);
            if !was_held && matcher.matches_current_modifiers(inner) {
                inner.matched_held.store(true, Ordering::SeqCst);
                if let Err(e) = inner
                    .tx
                    .send(ComboHotkeyEvent::Pressed { at: Instant::now() })
                {
                    log::warn!("[combo-hotkey] pass-through event send failed: {e}");
                }
            }
        } else if is_up {
            let was_held = inner.primary_held.swap(false, Ordering::SeqCst);
            let matched = inner.matched_held.swap(false, Ordering::SeqCst);
            if was_held && matched {
                let _ = inner
                    .tx
                    .send(ComboHotkeyEvent::Released { at: Instant::now() });
            }
        }
    }

    fn update_modifier_state(inner: &Arc<Inner>, vk_code: u32, is_down: bool, is_up: bool) {
        let value = if is_down {
            Some(true)
        } else if is_up {
            Some(false)
        } else {
            None
        };
        let Some(value) = value else {
            return;
        };
        match vk_code {
            VK_CONTROL | VK_LCONTROL | VK_RCONTROL => {
                inner.ctrl_held.store(value, Ordering::SeqCst)
            }
            VK_MENU | VK_LMENU | VK_RMENU => inner.alt_held.store(value, Ordering::SeqCst),
            VK_SHIFT | VK_LSHIFT | VK_RSHIFT => inner.shift_held.store(value, Ordering::SeqCst),
            VK_LWIN | VK_RWIN => inner.super_held.store(value, Ordering::SeqCst),
            _ => {}
        }
    }

    #[derive(Clone, Copy)]
    struct BindingMatcher {
        vk_code: u32,
        ctrl: bool,
        alt: bool,
        shift: bool,
        super_key: bool,
    }

    impl BindingMatcher {
        fn from_binding(binding: &ShortcutBinding) -> Result<Self, ComboHotkeyError> {
            let hotkey = parse_binding(binding)?;
            Ok(Self {
                vk_code: code_to_vk(hotkey.key)
                    .ok_or_else(|| ComboHotkeyError::UnsupportedKey(binding.primary.clone()))?,
                ctrl: hotkey.mods.contains(Modifiers::CONTROL),
                alt: hotkey.mods.contains(Modifiers::ALT),
                shift: hotkey.mods.contains(Modifiers::SHIFT),
                super_key: hotkey.mods.contains(Modifiers::SUPER)
                    || hotkey.mods.contains(Modifiers::META),
            })
        }

        fn matches_current_modifiers(self, inner: &Inner) -> bool {
            inner.ctrl_held.load(Ordering::SeqCst) == self.ctrl
                && inner.alt_held.load(Ordering::SeqCst) == self.alt
                && inner.shift_held.load(Ordering::SeqCst) == self.shift
                && inner.super_held.load(Ordering::SeqCst) == self.super_key
        }
    }

    fn code_to_vk(code: Code) -> Option<u32> {
        match code {
            Code::KeyA => Some(0x41),
            Code::KeyB => Some(0x42),
            Code::KeyC => Some(0x43),
            Code::KeyD => Some(0x44),
            Code::KeyE => Some(0x45),
            Code::KeyF => Some(0x46),
            Code::KeyG => Some(0x47),
            Code::KeyH => Some(0x48),
            Code::KeyI => Some(0x49),
            Code::KeyJ => Some(0x4A),
            Code::KeyK => Some(0x4B),
            Code::KeyL => Some(0x4C),
            Code::KeyM => Some(0x4D),
            Code::KeyN => Some(0x4E),
            Code::KeyO => Some(0x4F),
            Code::KeyP => Some(0x50),
            Code::KeyQ => Some(0x51),
            Code::KeyR => Some(0x52),
            Code::KeyS => Some(0x53),
            Code::KeyT => Some(0x54),
            Code::KeyU => Some(0x55),
            Code::KeyV => Some(0x56),
            Code::KeyW => Some(0x57),
            Code::KeyX => Some(0x58),
            Code::KeyY => Some(0x59),
            Code::KeyZ => Some(0x5A),
            Code::Digit0 => Some(0x30),
            Code::Digit1 => Some(0x31),
            Code::Digit2 => Some(0x32),
            Code::Digit3 => Some(0x33),
            Code::Digit4 => Some(0x34),
            Code::Digit5 => Some(0x35),
            Code::Digit6 => Some(0x36),
            Code::Digit7 => Some(0x37),
            Code::Digit8 => Some(0x38),
            Code::Digit9 => Some(0x39),
            Code::Enter => Some(VK_RETURN),
            Code::Tab => Some(VK_TAB),
            Code::Escape => Some(VK_ESCAPE),
            Code::Space => Some(VK_SPACE),
            Code::Backspace => Some(VK_BACK),
            Code::Delete => Some(VK_DELETE),
            Code::Home => Some(VK_HOME),
            Code::End => Some(VK_END),
            Code::PageUp => Some(VK_PRIOR),
            Code::PageDown => Some(VK_NEXT),
            Code::ArrowUp => Some(VK_UP),
            Code::ArrowDown => Some(VK_DOWN),
            Code::ArrowLeft => Some(VK_LEFT),
            Code::ArrowRight => Some(VK_RIGHT),
            Code::F1 => Some(0x70),
            Code::F2 => Some(0x71),
            Code::F3 => Some(0x72),
            Code::F4 => Some(0x73),
            Code::F5 => Some(0x74),
            Code::F6 => Some(0x75),
            Code::F7 => Some(0x76),
            Code::F8 => Some(0x77),
            Code::F9 => Some(0x78),
            Code::F10 => Some(0x79),
            Code::F11 => Some(0x7A),
            Code::F12 => Some(0x7B),
            Code::Semicolon => Some(0xBA),
            Code::Equal => Some(0xBB),
            Code::Comma => Some(0xBC),
            Code::Minus => Some(0xBD),
            Code::Period => Some(0xBE),
            Code::Slash => Some(0xBF),
            Code::Backquote => Some(0xC0),
            Code::BracketLeft => Some(0xDB),
            Code::Backslash => Some(0xDC),
            Code::BracketRight => Some(0xDD),
            Code::Quote => Some(0xDE),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn test_inner(
            binding: ShortcutBinding,
        ) -> (Arc<Inner>, std::sync::mpsc::Receiver<ComboHotkeyEvent>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let matcher = BindingMatcher::from_binding(&binding).unwrap();
            (
                Arc::new(Inner {
                    binding: parking_lot::RwLock::new(matcher),
                    tx,
                    thread_id: parking_lot::Mutex::new(None),
                    ctrl_held: AtomicBool::new(false),
                    alt_held: AtomicBool::new(false),
                    shift_held: AtomicBool::new(false),
                    super_held: AtomicBool::new(false),
                    primary_held: AtomicBool::new(false),
                    matched_held: AtomicBool::new(false),
                    installed: AtomicBool::new(false),
                }),
                rx,
            )
        }

        #[test]
        fn enter_passthrough_dispatch_does_not_require_modifiers() {
            let (inner, rx) = test_inner(ShortcutBinding {
                primary: "Enter".into(),
                modifiers: vec![],
            });

            dispatch_keyboard_event(&inner, VK_RETURN, WM_KEYDOWN);
            dispatch_keyboard_event(&inner, VK_RETURN, WM_KEYDOWN);
            dispatch_keyboard_event(&inner, VK_RETURN, WM_KEYUP);

            assert!(matches!(
                rx.recv().unwrap(),
                ComboHotkeyEvent::Pressed { .. }
            ));
            assert!(matches!(
                rx.recv().unwrap(),
                ComboHotkeyEvent::Released { .. }
            ));
            assert!(rx.try_recv().is_err());
        }

        #[test]
        fn enter_passthrough_ignores_shift_enter() {
            let (inner, rx) = test_inner(ShortcutBinding {
                primary: "Enter".into(),
                modifiers: vec![],
            });

            dispatch_keyboard_event(&inner, VK_SHIFT, WM_KEYDOWN);
            dispatch_keyboard_event(&inner, VK_RETURN, WM_KEYDOWN);
            dispatch_keyboard_event(&inner, VK_RETURN, WM_KEYUP);
            dispatch_keyboard_event(&inner, VK_SHIFT, WM_KEYUP);

            assert!(rx.try_recv().is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::{Code, Modifiers};

    #[test]
    fn parse_cmd_shift_d() {
        let binding = ShortcutBinding {
            primary: "D".into(),
            modifiers: vec!["cmd".into(), "shift".into()],
        };
        let parsed = parse_binding(&binding).expect("binding parses");
        #[cfg(target_os = "windows")]
        assert!(parsed.mods.contains(Modifiers::CONTROL));
        #[cfg(not(target_os = "windows"))]
        assert!(parsed.mods.contains(Modifiers::SUPER));
        assert!(parsed.mods.contains(Modifiers::SHIFT));
        assert_eq!(parsed.key, Code::KeyD);
    }

    #[test]
    fn parse_ctrl_shift_space() {
        let binding = ShortcutBinding {
            primary: "Space".into(),
            modifiers: vec!["ctrl".into(), "shift".into()],
        };
        let parsed = parse_binding(&binding).expect("binding parses");
        assert!(parsed.mods.contains(Modifiers::CONTROL));
        assert!(parsed.mods.contains(Modifiers::SHIFT));
        assert_eq!(parsed.key, Code::Space);
    }

    #[test]
    fn unsupported_modifier_rejected() {
        let binding = ShortcutBinding {
            primary: "D".into(),
            modifiers: vec!["hyper".into()],
        };
        assert!(matches!(
            parse_binding(&binding),
            Err(ComboHotkeyError::UnsupportedModifier(_))
        ));
    }

    #[test]
    fn empty_primary_rejected() {
        let binding = ShortcutBinding {
            primary: "".into(),
            modifiers: vec!["cmd".into()],
        };
        assert!(matches!(
            parse_binding(&binding),
            Err(ComboHotkeyError::UnsupportedKey(_))
        ));
    }

    #[test]
    fn bare_shift_is_rejected_for_combo_hotkey() {
        let binding = ShortcutBinding {
            primary: "Shift".into(),
            modifiers: vec![],
        };
        assert!(matches!(
            validate_binding(&binding),
            Err(ComboHotkeyError::UnsupportedKey(_))
        ));
    }

    #[test]
    fn legacy_modifier_only_is_rejected_for_combo_hotkey() {
        let binding = ShortcutBinding {
            primary: "RightOption".into(),
            modifiers: vec![],
        };
        assert!(matches!(
            validate_binding(&binding),
            Err(ComboHotkeyError::UnsupportedKey(_))
        ));
    }

    #[test]
    fn forward_loop_ignores_unrelated_hotkey_ids() {
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let (out_tx, out_rx) = std::sync::mpsc::channel();

        event_tx
            .send(GlobalHotKeyEvent {
                id: 7,
                state: HotKeyState::Pressed,
            })
            .unwrap();
        event_tx
            .send(GlobalHotKeyEvent {
                id: 8,
                state: HotKeyState::Released,
            })
            .unwrap();
        event_tx
            .send(GlobalHotKeyEvent {
                id: 8,
                state: HotKeyState::Pressed,
            })
            .unwrap();
        drop(event_tx);

        forward_loop(8, event_rx, out_tx);

        assert!(matches!(
            out_rx.recv().unwrap(),
            ComboHotkeyEvent::Released { .. }
        ));
        assert!(matches!(
            out_rx.recv().unwrap(),
            ComboHotkeyEvent::Pressed { .. }
        ));
        assert!(out_rx.try_recv().is_err());
    }
}
