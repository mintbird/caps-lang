# End-to-end test: drives the running (installed) caps-lang with injected keys and checks
# Caps Lock state, the input language and the keys the app sends.
# Keep this terminal focused and don't type while it runs.
param([switch]$Long)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$built = Join-Path $root 'target\release\caps-lang.exe'
$installed = Join-Path $env:ProgramFiles 'caps-lang\caps-lang.exe'

Add-Type @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Threading;
public static class E2E {
  public const ulong TAG = 0x43415053;
  delegate IntPtr Proc(int code, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] struct KB { public uint vk, scan, flags, time; public UIntPtr extra; }
  [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr h; public uint m; public IntPtr w, l; public uint t; public int x, y; }
  [DllImport("user32.dll")] static extern IntPtr SetWindowsHookExW(int id, Proc p, IntPtr mod, uint tid);
  [DllImport("user32.dll")] static extern bool UnhookWindowsHookEx(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr CallNextHookEx(IntPtr h, int c, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] static extern int GetMessageW(out MSG m, IntPtr h, uint a, uint b);
  [DllImport("user32.dll")] static extern bool PostThreadMessageW(uint tid, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] static extern short GetKeyState(int vk);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint tid);
  [DllImport("user32.dll")] static extern int GetKeyboardLayoutList(int n, IntPtr[] list);
  [DllImport("user32.dll")] static extern bool AttachThreadInput(uint a, uint b, bool f);
  [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
  [DllImport("kernel32.dll")] static extern IntPtr GetModuleHandleW(string n);

  public class Ev { public bool Down; public uint Vk, Scan, Flags; public ulong Extra; }
  static readonly List<Ev> log = new List<Ev>();
  static Proc keep; static uint hookThread;

  static IntPtr Cb(int c, IntPtr w, IntPtr l) {
    var k = (KB)Marshal.PtrToStructure(l, typeof(KB));
    lock (log) log.Add(new Ev { Down = (long)w == 0x100 || (long)w == 0x104, Vk = k.vk, Scan = k.scan, Flags = k.flags, Extra = (ulong)k.extra });
    return CallNextHookEx(IntPtr.Zero, c, w, l);
  }
  public static void StartLog() {
    var ready = new ManualResetEvent(false);
    var t = new Thread(() => {
      hookThread = GetCurrentThreadId(); keep = Cb;
      var h = SetWindowsHookExW(13, keep, GetModuleHandleW(null), 0);
      ready.Set(); MSG m; while (GetMessageW(out m, IntPtr.Zero, 0, 0) > 0) {}
      UnhookWindowsHookEx(h);
    });
    t.IsBackground = true; t.Start(); ready.WaitOne();
  }
  public static void StopLog() { PostThreadMessageW(hookThread, 0x12, IntPtr.Zero, IntPtr.Zero); }
  public static void ClearLog() { lock (log) log.Clear(); }
  public static Ev[] Log() { lock (log) return log.ToArray(); }

  static uint FgThread() { return GetWindowThreadProcessId(GetForegroundWindow(), IntPtr.Zero); }
  /// Caps Lock as the foreground window sees it.
  public static bool CapsOn() {
    uint fg = FgThread(), me = GetCurrentThreadId();
    bool a = AttachThreadInput(me, fg, true); bool on = (GetKeyState(0x14) & 1) != 0;
    if (a) AttachThreadInput(me, fg, false); return on;
  }
  public static long Lang() { return GetKeyboardLayout(FgThread()).ToInt64(); }
  public static int LangCount() { return GetKeyboardLayoutList(0, null); }

  /// Untagged injection, like the on-screen keyboard.
  public static void Key(byte vk, byte scan, bool up) { keybd_event(vk, scan, up ? 2u : 0u, UIntPtr.Zero); Thread.Sleep(40); }
  /// Tagged, so the app lets it through.
  public static void RawCapsTap() { var t = (UIntPtr)TAG; keybd_event(0x14, 0x3A, 0, t); Thread.Sleep(30); keybd_event(0x14, 0x3A, 2, t); Thread.Sleep(40); }
}
'@

# ---------- helpers ----------
$results = [System.Collections.Generic.List[object]]::new()
function Result($name, $pass, $detail) {
    $results.Add([pscustomobject]@{ Result = if ($pass) { 'PASS' } else { 'FAIL' }; Case = $name; Detail = $detail })
}
function Skip($name, $why) { $results.Add([pscustomobject]@{ Result = 'SKIP'; Case = $name; Detail = $why }) }

function Settle { Start-Sleep -Milliseconds 500 }
function Caps { [E2E]::Key(0x14, 0x3A, $false); [E2E]::Key(0x14, 0x3A, $true); Settle }
function With($vk, $scan, [scriptblock]$body) { [E2E]::Key($vk, $scan, $false); & $body; [E2E]::Key($vk, $scan, $true); Settle }
function ShiftCaps { With 0xA0 0x2A { [E2E]::Key(0x14, 0x3A, $false); [E2E]::Key(0x14, 0x3A, $true) } }
function ShiftTap { [E2E]::Key(0xA0, 0x2A, $false); [E2E]::Key(0xA0, 0x2A, $true); Settle }
# Win key-downs the app sent (tagged) since the last ClearLog = language switches.
function Switches { @([E2E]::Log() | Where-Object { $_.Down -and $_.Vk -eq 0x5B -and $_.Extra -eq [E2E]::TAG }).Count }
function CapsOff {
    if ([E2E]::CapsOn()) { if ($shiftLock) { ShiftTap } else { [E2E]::RawCapsTap(); Settle } }
}
function Case($name, [scriptblock]$body) {
    CapsOff; [E2E]::ClearLog()
    try { & $body } catch { Result $name $false "error: $_" }
}

# ---------- pre-checks ----------
if (-not (Get-Process caps-lang -ErrorAction SilentlyContinue)) {
    Write-Host 'caps-lang is not running. Install it first: .\target\release\caps-lang.exe --install' -ForegroundColor Red
    exit 2
}
if (-not (Test-Path $built)) { Write-Host "No build at $built. Run: cargo build --release" -ForegroundColor Red; exit 2 }
if (-not (Test-Path $installed) -or (Get-FileHash $installed).Hash -ne (Get-FileHash $built).Hash) {
    Write-Host 'The installed caps-lang is not the current build. Reinstall: .\target\release\caps-lang.exe --install' -ForegroundColor Red
    exit 2
}
$attrs = (Get-ItemProperty 'HKCU:\Keyboard Layout' -Name Attributes -ErrorAction SilentlyContinue).Attributes
$shiftLock = ($attrs -band 0x10000) -ne 0
$langs = [E2E]::LangCount()
Write-Host "Caps Lock mode: $(if ($shiftLock) { 'Press SHIFT to turn off' } else { 'normal' }); input languages: $langs"
Write-Host 'Keep this window focused and do not type for ~15 s...' -ForegroundColor Yellow
Start-Sleep -Seconds 2

$startLang = [E2E]::Lang()
$startCaps = [E2E]::CapsOn()
[E2E]::StartLog()

try {
    Case '1. Caps switches language, Caps Lock stays off' {
        $before = [E2E]::Lang(); Caps
        $n = Switches; $caps = [E2E]::CapsOn(); $changed = [E2E]::Lang() -ne $before
        $ok = $n -eq 1 -and -not $caps -and ($langs -lt 2 -or $changed)
        Result '1. Caps switches language, Caps Lock stays off' $ok "switches=$n capsOn=$caps languageChanged=$changed"
    }

    Case '2. Holding Caps switches once' {
        1..5 | ForEach-Object { [E2E]::Key(0x14, 0x3A, $false) }
        [E2E]::Key(0x14, 0x3A, $true); Settle
        $n = Switches
        Result '2. Holding Caps switches once' ($n -eq 1) "switches=$n (expected 1)"
    }

    Case '3. Shift+Caps turns Caps Lock on, no switch' {
        ShiftCaps
        $n = Switches; $caps = [E2E]::CapsOn()
        Result '3. Shift+Caps turns Caps Lock on, no switch' ($caps -and $n -eq 0) "capsOn=$caps switches=$n"
    }

    Case '4. Caps with Caps Lock on: off + switch' {
        ShiftCaps
        if (-not [E2E]::CapsOn()) { throw 'could not turn Caps Lock on (see case 3)' }
        [E2E]::ClearLog(); Caps
        $n = Switches; $caps = [E2E]::CapsOn()
        Result '4. Caps with Caps Lock on: off + switch' (-not $caps -and $n -eq 1) "capsOn=$caps switches=$n"
    }

    if ($shiftLock) {
        Case '5. Shift turns Caps Lock off' {
            ShiftCaps
            if (-not [E2E]::CapsOn()) { throw 'could not turn Caps Lock on (see case 3)' }
            ShiftTap
            $caps = [E2E]::CapsOn()
            Result '5. Shift turns Caps Lock off' (-not $caps) "capsOn=$caps"
        }
    } else {
        Case '5. Shift+Caps again turns Caps Lock off' {
            ShiftCaps; ShiftCaps
            $caps = [E2E]::CapsOn(); $n = Switches
            Result '5. Shift+Caps again turns Caps Lock off' (-not $caps -and $n -eq 0) "capsOn=$caps switches=$n"
        }
    }

    Case '6. Ctrl+Caps and Alt+Caps do nothing' {
        $before = [E2E]::Lang()
        With 0xA2 0x1D { [E2E]::Key(0x14, 0x3A, $false); [E2E]::Key(0x14, 0x3A, $true) }
        With 0xA4 0x38 { [E2E]::Key(0x14, 0x3A, $false); [E2E]::Key(0x14, 0x3A, $true) }
        $n = Switches; $caps = [E2E]::CapsOn(); $changed = [E2E]::Lang() -ne $before
        Result '6. Ctrl+Caps and Alt+Caps do nothing' ($n -eq 0 -and -not $caps -and -not $changed) "switches=$n capsOn=$caps languageChanged=$changed"
    }

    Case '7. Sent keys have real scan codes' {
        Caps
        $sent = [E2E]::Log() | Where-Object { $_.Extra -eq [E2E]::TAG -and ($_.Vk -eq 0x5B -or $_.Vk -eq 0x20) }
        $win = @($sent | Where-Object Vk -eq 0x5B); $space = @($sent | Where-Object Vk -eq 0x20)
        $winOk = $win.Count -eq 2 -and -not ($win | Where-Object { ($_.Scan -band 0xFF) -ne 0x5B -or ($_.Flags -band 1) -eq 0 })
        $spaceOk = $space.Count -eq 2 -and -not ($space | Where-Object { $_.Scan -ne 0x39 })
        Result '7. Sent keys have real scan codes' ($winOk -and $spaceOk) "win=$($win.Count) events ok=$winOk; space=$($space.Count) events ok=$spaceOk"
    }

    if ($Long) {
        Write-Host 'Waiting 130 s for two hook re-arms...' -ForegroundColor Yellow
        Start-Sleep -Seconds 130
        Case '8. Still works after hook re-arm' {
            Caps
            $n = Switches
            Result '8. Still works after hook re-arm' ($n -eq 1) "switches=$n"
        }
    } else {
        Skip '8. Still works after hook re-arm' 'run with -Long'
    }
}
finally {
    # Restore Caps Lock and the starting language.
    CapsOff
    if ($startCaps -and -not [E2E]::CapsOn()) { ShiftCaps }
    for ($i = 0; $i -lt $langs -and [E2E]::Lang() -ne $startLang; $i++) { Caps }
    [E2E]::StopLog()
}

$results | Format-Table -AutoSize | Out-String | Write-Host
Write-Host 'Not covered: installer (needs UAC), the other Caps Lock mode, lock screen / RDP / games.'
$failed = @($results | Where-Object Result -eq 'FAIL').Count
if ($failed) { Write-Host "$failed failed" -ForegroundColor Red; exit 1 }
Write-Host 'All passed' -ForegroundColor Green
exit 0
