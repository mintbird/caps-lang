#![windows_subsystem = "windows"]

use std::mem::{size_of, size_of_val, zeroed};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, GetCurrentThreadId, GetExitCodeProcess, OpenProcess,
    OpenProcessToken, WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VK_CAPITAL, VK_CONTROL,
    VK_LSHIFT, VK_LWIN, VK_MENU, VK_SHIFT, VK_SPACE,
};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetCoalescableTimer, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN,
    WM_SYSKEYDOWN, WM_TIMER,
};

/// Tags our own synthetic input so the hook can ignore it.
const TAG: usize = 0x4341_5053;
const TASK: &str = "caps-lang";
const EXE: &str = "caps-lang.exe";
/// A held key repeats well within this; a longer gap means we missed its key-up.
const STALE_MS: u32 = 1500;
/// How often the hook is re-armed; Windows may batch the wake-up within the same span again.
const REARM_MS: u32 = 60_000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const WM_SWITCH: u32 = WM_APP;
const WM_TOGGLE_CAPS: u32 = WM_APP + 1;
/// Keyboard layout attribute behind "Press the SHIFT key to turn off Caps Lock".
const KLLF_SHIFTLOCK: u32 = 0x0001_0000;
/// Scan-code bit Windows sets on key events it generates itself.
const SCANCODE_SIMULATED: u32 = 0x200;

static HELD: AtomicBool = AtomicBool::new(false);
static LAST_DOWN: AtomicU32 = AtomicU32::new(0);
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);

fn key(vk: u16, up: bool) -> INPUT {
    // Real scan codes, so RDP, VM consoles and raw-input games see actual keys.
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if vk == VK_LWIN {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: TAG,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
}

fn caps_on() -> bool {
    (unsafe { GetKeyState(VK_CAPITAL as i32) } & 1) != 0
}

/// Toggles Caps Lock; turns it on only, under "Press Shift to turn off Caps Lock".
fn caps_tap() -> [INPUT; 2] {
    [key(VK_CAPITAL, false), key(VK_CAPITAL, true)]
}

/// Windows setting "Press the SHIFT key to turn off Caps Lock".
fn shift_turns_off_caps() -> bool {
    let mut attributes = 0u32;
    let mut size = size_of::<u32>() as u32;
    let found = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide("Keyboard Layout").as_ptr(),
            wide("Attributes").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut attributes as *mut u32).cast(),
            &mut size,
        )
    } == 0;
    found && attributes & KLLF_SHIFTLOCK != 0
}

fn caps_off() {
    if caps_on() {
        let tap = if shift_turns_off_caps() {
            [key(VK_LSHIFT, false), key(VK_LSHIFT, true)]
        } else {
            caps_tap()
        };
        send(&tap);
    }
}

fn switch_language() {
    let win_space = [
        key(VK_LWIN, false),
        key(VK_SPACE, false),
        key(VK_SPACE, true),
        key(VK_LWIN, true),
    ];
    // Caps Lock can get switched on where the hook can't run, e.g. the lock screen or UAC.
    caps_off();
    send(&win_space);
}

fn held(vk: u16) -> bool {
    (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
}

/// Whether the hook takes this key event over (and swallows it).
fn ours_to_handle(vk: u32, extra: usize, scan: u32) -> bool {
    // Injected Caps counts too, so the on-screen/touch keyboard and remappers switch language.
    // Caps that Windows simulates itself (Shift turning Caps Lock off) passes through.
    vk == VK_CAPITAL as u32 && extra != TAG && scan & SCANCODE_SIMULATED == 0
}

/// Whether a Caps event is a new press, given whether Caps was already held and when it went down.
fn is_press(down: bool, time: u32, was_held: bool, last_down: u32) -> bool {
    if down {
        // swallow auto-repeat; act once per press
        !was_held || time.wrapping_sub(last_down) > STALE_MS
    } else {
        // Under "Press Shift to turn off Caps Lock", Windows drops the key-down while
        // Caps Lock is on, so a key-up without one is the press.
        !was_held
    }
}

/// Caps switches language, Shift+Caps toggles real Caps Lock, Ctrl/Alt+Caps does nothing.
fn action(shift: bool, ctrl: bool, alt: bool) -> Option<u32> {
    match (shift, ctrl || alt) {
        (_, true) => None,
        (true, false) => Some(WM_TOGGLE_CAPS),
        (false, false) => Some(WM_SWITCH),
    }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(lparam as *const KBDLLHOOKSTRUCT);
        if ours_to_handle(k.vkCode, k.dwExtraInfo, k.scanCode) {
            let down = wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN;
            let was_held = HELD.swap(down, Ordering::Relaxed);
            let last_down = LAST_DOWN.load(Ordering::Relaxed);
            if down {
                LAST_DOWN.store(k.time, Ordering::Relaxed);
            }
            if is_press(down, k.time, was_held, last_down) {
                if let Some(msg) = action(held(VK_SHIFT), held(VK_CONTROL), held(VK_MENU)) {
                    // Sending happens in the message loop: a slow hook gets dropped by Windows.
                    PostThreadMessageW(MAIN_THREAD.load(Ordering::Relaxed), msg, 0, 0);
                }
            }
            return 1;
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

fn set_hook() -> HHOOK {
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), null_mut(), 0) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    wide(&p.to_string_lossy())
}

fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_or(false, |o| o.status.success())
}

fn install_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("ProgramFiles")?).join(TASK))
}

fn session_of(pid: u32) -> Option<u32> {
    let mut id = 0;
    (unsafe { ProcessIdToSessionId(pid, &mut id) } != 0).then_some(id)
}

/// Ends running copies of the app, except this process and the launcher that spawned it.
/// With a session, only copies in that session are ended.
fn stop_app(launcher: u32, session: Option<u32>) {
    let me = format!("PID ne {}", unsafe { GetCurrentProcessId() });
    let parent = format!("PID ne {launcher}");
    let mut args = vec!["/F", "/IM", EXE, "/FI", &me, "/FI", &parent];
    let in_session = session.map(|s| format!("SESSION eq {s}"));
    if let Some(filter) = &in_session {
        args.extend(["/FI", filter]);
    }
    run("taskkill", &args);
}

/// SID of the user running the launcher: the logged-on user, not the admin who approved UAC.
fn launcher_sid(launcher: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, launcher);
        if process.is_null() {
            return None;
        }
        let mut token: HANDLE = null_mut();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        CloseHandle(process);
        if opened == 0 {
            return None;
        }
        let mut buf = [0u64; 64];
        let mut len = 0;
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: PWSTR = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
            return None;
        }
        let len = (0..).take_while(|&i| *s.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(s, len));
        LocalFree(s.cast());
        Some(sid)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn task_xml(user: &str, exe: &str) -> String {
    let user = xml_escape(user);
    let exe = xml_escape(exe);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command></Exec></Actions>
</Task>"#
    )
}

fn delete_at_reboot(path: &Path) -> bool {
    unsafe { MoveFileExW(wide_path(path).as_ptr(), null(), MOVEFILE_DELAY_UNTIL_REBOOT) != 0 }
}

/// Deletes `file`; if it is in use (a running exe), moves it into `aside` and deletes it at reboot.
fn remove_or_defer(file: &Path, aside: &Path) -> bool {
    if std::fs::remove_file(file).is_ok() || !file.exists() {
        return true;
    }
    let moved = aside.join(format!("caps-lang-{}.old", unsafe { GetCurrentProcessId() }));
    if std::fs::rename(file, &moved).is_ok() {
        return delete_at_reboot(&moved);
    }
    delete_at_reboot(file)
}

/// Copies the exe to Program Files (admin-only writable, since the task runs elevated),
/// registers the logon task and starts the app.
fn install(launcher: u32) -> Option<()> {
    let src = std::env::current_exe().ok()?;
    let dir = install_dir()?;
    let dest = dir.join(EXE);
    let user = launcher_sid(launcher)?;

    stop_app(launcher, session_of(launcher));
    if src != dest {
        std::fs::create_dir_all(&dir).ok()?;
        // Copies still running in other sessions keep the old exe locked, so move it aside.
        if !remove_or_defer(&dest, &dir) {
            return None;
        }
        std::fs::copy(&src, &dest).ok()?;
    }

    let xml = task_xml(&user, &dest.to_string_lossy());
    let bytes: Vec<u8> = std::iter::once(0xFEFF_u16)
        .chain(xml.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    // Inside the admin-only install dir, so nothing unprivileged can swap it before schtasks reads it.
    let file = dir.join("task.xml");
    std::fs::write(&file, bytes).ok()?;
    let created = run("schtasks", &["/Create", "/F", "/TN", TASK, "/XML", &file.to_string_lossy()]);
    let _ = std::fs::remove_file(&file);
    if !created {
        return None;
    }
    run("schtasks", &["/Run", "/TN", TASK]);
    Some(())
}

fn uninstall(launcher: u32) -> Option<()> {
    let removed = run("schtasks", &["/Delete", "/F", "/TN", TASK]);
    // The exe is shared, so stop the app in every session.
    stop_app(launcher, None);
    let mut cleaned = true;
    if let Some(dir) = install_dir() {
        let aside = std::env::var_os("SystemRoot").map_or_else(|| dir.clone(), |r| PathBuf::from(r).join("Temp"));
        cleaned = remove_or_defer(&dir.join(EXE), &aside);
        if dir.exists() && std::fs::remove_dir(&dir).is_err() {
            // Leftover files are deleted at reboot first, then the folder.
            cleaned &= delete_at_reboot(&dir);
        }
    }
    (removed && cleaned).then_some(())
}

/// Re-launches this exe elevated (UAC prompt) and waits for its exit code.
/// None = the user declined the prompt or launch failed.
fn run_elevated(arg: &str) -> Option<u32> {
    unsafe {
        let exe = wide_path(&std::env::current_exe().ok()?);
        let verb = wide("runas");
        let params = wide(&format!("{arg} --elevated {}", GetCurrentProcessId()));
        let mut info: SHELLEXECUTEINFOW = zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = exe.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = 0; // hidden
        if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
            return None;
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        Some(code)
    }
}

fn manage(install_mode: bool, launcher: Option<u32>) -> ! {
    let code = if let Some(launcher) = launcher {
        let ok = if install_mode { install(launcher) } else { uninstall(launcher) };
        i32::from(ok.is_none())
    } else {
        run_elevated(if install_mode { "--install" } else { "--uninstall" }).map_or(1, |c| c as i32)
    };
    std::process::exit(code)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // "--elevated <launcher pid>" marks the elevated child
    let launcher = match args.get(1).map(String::as_str) {
        Some("--elevated") => Some(args.get(2).and_then(|p| p.parse().ok()).unwrap_or(0)),
        _ => None,
    };
    match args.first().map(String::as_str) {
        Some("--install") => manage(true, launcher),
        Some("--uninstall") => manage(false, launcher),
        Some(_) => return,
        None => {}
    }

    unsafe {
        let name = wide("Local\\caps-lang");
        let mutex = CreateMutexW(null(), 0, name.as_ptr());
        // Without a working guard, two hooks would each send Win+Space and cancel out.
        if mutex.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        // Turn Caps Lock off once at start.
        caps_off();

        MAIN_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
        let mut hook = set_hook();
        if hook.is_null() {
            return;
        }
        SetCoalescableTimer(null_mut(), 0, REARM_MS, None, REARM_MS);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_SWITCH => switch_language(),
                WM_TOGGLE_CAPS => send(&caps_tap()),
                // Windows silently drops a hook that once misses LowLevelHooksTimeout; re-arming
                // brings it back. Both run on this thread, so no key is seen by two hooks.
                WM_TIMER => {
                    let fresh = set_hook();
                    if !fresh.is_null() {
                        UnhookWindowsHookEx(hook);
                        hook = fresh;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS: u32 = VK_CAPITAL as u32;
    const CAPS_SCAN: u32 = 0x3A;

    #[test]
    fn handles_plain_and_injected_caps() {
        assert!(ours_to_handle(CAPS, 0, CAPS_SCAN));
        // on-screen keyboard / remappers inject with their own or no extra info
        assert!(ours_to_handle(CAPS, 0x1234, CAPS_SCAN));
    }

    #[test]
    fn ignores_other_keys_own_input_and_windows_simulated_caps() {
        assert!(!ours_to_handle(VK_SHIFT as u32, 0, 0x2A));
        assert!(!ours_to_handle(VK_SPACE as u32, 0, 0x39));
        assert!(!ours_to_handle(CAPS, TAG, CAPS_SCAN));
        // seen on a real machine: Shift turning Caps Lock off arrives as scan 0x23A
        assert!(!ours_to_handle(CAPS, 0, 0x23A));
    }

    #[test]
    fn first_key_down_is_a_press() {
        assert!(is_press(true, 1000, false, 0));
    }

    #[test]
    fn auto_repeat_is_not_a_press() {
        assert!(!is_press(true, 1030, true, 1000));
        assert!(!is_press(true, 1000 + STALE_MS, true, 1000));
    }

    #[test]
    fn key_down_after_missed_key_up_is_a_press() {
        assert!(is_press(true, 1001 + STALE_MS, true, 1000));
    }

    #[test]
    fn stale_check_survives_tick_count_wrap() {
        assert!(!is_press(true, 10, true, u32::MAX - 10));
    }

    #[test]
    fn key_up_after_key_down_is_not_a_press() {
        assert!(!is_press(false, 1100, true, 1000));
    }

    #[test]
    fn key_up_without_key_down_is_a_press() {
        // "Press Shift to turn off Caps Lock": Windows drops the key-down while Caps Lock is on
        assert!(is_press(false, 1100, false, 1000));
    }

    #[test]
    fn caps_alone_switches_language() {
        assert_eq!(action(false, false, false), Some(WM_SWITCH));
    }

    #[test]
    fn shift_caps_toggles_caps_lock() {
        assert_eq!(action(true, false, false), Some(WM_TOGGLE_CAPS));
    }

    #[test]
    fn ctrl_or_alt_caps_does_nothing() {
        for shift in [false, true] {
            assert_eq!(action(shift, true, false), None);
            assert_eq!(action(shift, false, true), None);
            assert_eq!(action(shift, true, true), None);
        }
    }

    fn ki(input: &INPUT) -> KEYBDINPUT {
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        unsafe { input.Anonymous.ki }
    }

    #[test]
    fn sent_keys_carry_scan_codes_and_tag() {
        for vk in [VK_LWIN, VK_SPACE, VK_CAPITAL, VK_LSHIFT] {
            for up in [false, true] {
                let k = ki(&key(vk, up));
                assert_eq!(k.wVk, vk);
                assert_ne!(k.wScan, 0, "vk {vk:#x} has no scan code");
                assert_eq!(k.dwExtraInfo, TAG);
                assert_eq!(k.dwFlags & KEYEVENTF_KEYUP != 0, up);
                assert_eq!(k.dwFlags & KEYEVENTF_EXTENDEDKEY != 0, vk == VK_LWIN);
            }
        }
    }

    #[test]
    fn caps_tap_is_down_then_up() {
        let [down, up] = caps_tap();
        assert_eq!(ki(&down).dwFlags & KEYEVENTF_KEYUP, 0);
        assert_ne!(ki(&up).dwFlags & KEYEVENTF_KEYUP, 0);
    }

    #[test]
    fn xml_escape_covers_all_specials() {
        assert_eq!(xml_escape(r#"&<>"'"#), "&amp;&lt;&gt;&quot;&apos;");
        assert_eq!(xml_escape("Tom & Jerry"), "Tom &amp; Jerry");
        assert_eq!(xml_escape("plain"), "plain");
    }

    #[test]
    fn task_xml_uses_escaped_user_and_exe() {
        let sid = "S-1-5-21-1-2-3-1001";
        let xml = task_xml(sid, r"C:\Program Files\A & B\caps-lang.exe");
        assert_eq!(xml.matches(&format!("<UserId>{sid}</UserId>")).count(), 2);
        assert!(xml.contains(r"<Command>C:\Program Files\A &amp; B\caps-lang.exe</Command>"));
        assert!(!xml.contains(" & "));
    }
}
