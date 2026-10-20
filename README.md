# caps-lang: Caps Lock Keyboard Layout Switcher for Windows 11

A tiny (~330 KB), dependency-free background app written in Rust that lets you use **Caps Lock** instead of <kbd>Win</kbd>+<kbd>Space</kbd> to switch your input language. No window, no tray icon, no polling: it uses no CPU while you are not pressing Caps.

## 📝 Disclaimer

- This repo was created with AI (Claude Code).
- It is made for my own personal use and shared as is, with no warranty.
- It installs a keyboard hook and runs as administrator. If you don't trust the released `.exe`, read the source and [build it yourself](#build).

## Keys

| You press | What happens |
|---|---|
| <kbd>Caps</kbd> | Switches the input language (same as <kbd>Win</kbd>+<kbd>Space</kbd>). If Caps Lock is on, it is turned off too. |
| <kbd>Shift</kbd>+<kbd>Caps</kbd> | Turns real Caps Lock on/off, for when you really need it. |
| <kbd>Ctrl</kbd>+<kbd>Caps</kbd>, <kbd>Alt</kbd>+<kbd>Caps</kbd> | Nothing. |

Holding Caps switches only once. The On-Screen Keyboard and touch keyboard work too.

**"Press the SHIFT key to turn off Caps Lock"** (Settings → Time & language → Typing → Advanced keyboard settings → Input language hot keys → Advanced Key Settings) is supported: with it on, <kbd>Shift</kbd>+<kbd>Caps</kbd> turns Caps Lock on and <kbd>Shift</kbd> turns it off.

## Requirements

- Windows 11 (developed and tested there; Windows 10 should work but is untested)
- At least two input languages installed (Settings → Time & language → Language & region)
- To build: [Rust](https://rustup.rs/) with the MSVC toolchain

## Build

```powershell
cargo build --release
```

The app is `target\release\caps-lang.exe`. It is statically linked, so the single `.exe` is all you need.

## Install

```powershell
.\target\release\caps-lang.exe --install
```

Approve the UAC prompt. This:

1. copies the exe to `C:\Program Files\caps-lang\`
2. creates a scheduled task `caps-lang` that starts it at your logon
3. starts it right away (no need to log off)

Running `--install` again updates to the new build.

**Why admin?** The app runs elevated so Caps also works in apps running as administrator. Windows blocks a normal app from sending keys to elevated windows. That is also why it lives in Program Files, where only admins can change the exe.

## Uninstall

```powershell
& "C:\Program Files\caps-lang\caps-lang.exe" --uninstall
```

Approve the UAC prompt. The app stops immediately, the logon task is removed, and the files are deleted. If a file is still in use, it is deleted at the next reboot. Caps Lock goes back to normal right away.

## Try without installing

Run `target\release\caps-lang.exe` with no arguments. It runs until you sign out or end it in Task Manager. It won't work in admin windows, and only one copy runs at a time.

## How it works

- A low-level keyboard hook (`WH_KEYBOARD_LL`) swallows Caps key events and sends <kbd>Win</kbd>+<kbd>Space</kbd> with real scan codes, so Remote Desktop, VMs and games see real keys.
- Keys are sent from the message loop, not from inside the hook, so the hook stays fast. Windows silently removes hooks that respond too slowly, so the hook is also re-armed about once a minute on a coalesced timer.
- The app's own key events are tagged so it ignores them. Caps events that Windows generates itself (scan code bit `0x200`, used by "Press SHIFT to turn off Caps Lock") are passed through.
- The logon task is created for the user who ran the installer (by SID), even if a different admin account approved the UAC prompt.

## Tests

```powershell
.\test.ps1          # unit tests + end-to-end test (~20 s)
.\test.ps1 -Unit    # unit tests only, no keys pressed
.\test.ps1 -Long    # also checks the hook survives re-arming (+130 s)
```

- **Unit tests** (`cargo test`) cover the key-handling decisions, the keys sent, and the task XML.
- **End-to-end** (`tests\e2e.ps1`) presses keys against the *installed* app and checks Caps Lock state, the input language and the keys the app sends. It refuses to run if the installed app isn't the current build. Keep the terminal focused and don't type while it runs. It restores your language and Caps Lock state afterward.

Not covered by tests: the installer (needs UAC), the Caps Lock mode you're not using, the lock screen, RDP and games.

## Known limitations

- **Lock screen and UAC prompts:** no app can hook keys there, so Caps behaves normally. If Caps Lock gets turned on there, your next Caps press turns it off.
- **One install per PC:** the task name is shared. If two Windows accounts each run `--install`, the second replaces the first account's logon task.
- Caps Lock is always turned off when the app starts.

## Troubleshooting

- **Caps does nothing:** check that `caps-lang.exe` is running in Task Manager → Details. If it isn't, run `--install` again or start the task: `schtasks /Run /TN caps-lang`.
- **Language doesn't change:** make sure <kbd>Win</kbd>+<kbd>Space</kbd> itself switches languages and that at least two are installed.
- **Doesn't work in an admin window:** you are running the exe directly rather than the installed copy. Use `--install`.

## License

[MIT](LICENSE)
