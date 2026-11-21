# Lab: the caption buttons and the title bar on a real Windows window, driven with real mouse input.
$ErrorActionPreference = "Continue"
$out = "controls.txt"
"" | Out-File $out
function Say($text) { $text | Tee-Object -FilePath $out -Append }
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  public const uint LDOWN = 2, LUP = 4, RDOWN = 8, RUP = 16;
  public static void Click(int x, int y) { SetCursorPos(x, y); System.Threading.Thread.Sleep(150); mouse_event(LDOWN,0,0,0,UIntPtr.Zero); mouse_event(LUP,0,0,0,UIntPtr.Zero); }
  public static void RightClick(int x, int y) { SetCursorPos(x, y); System.Threading.Thread.Sleep(150); mouse_event(RDOWN,0,0,0,UIntPtr.Zero); mouse_event(RUP,0,0,0,UIntPtr.Zero); }
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
  public static void MoveTo(int x, int y) {
    // A real input event, in the 0..65535 absolute space, as a physical mouse makes.
    uint ax = (uint)(x * 65535 / (GetSystemMetrics(0) - 1)), ay = (uint)(y * 65535 / (GetSystemMetrics(1) - 1));
    mouse_event(0x0001 | 0x8000, ax, ay, 0, UIntPtr.Zero);
  }
  public static void Drag(int x, int y, int dx, int dy) {
    MoveTo(x, y); System.Threading.Thread.Sleep(150); mouse_event(LDOWN,0,0,0,UIntPtr.Zero);
    for (int i = 1; i <= 20; i++) { System.Threading.Thread.Sleep(30); MoveTo(x + dx * i / 20, y + dy * i / 20); }
    System.Threading.Thread.Sleep(300); mouse_event(LUP,0,0,0,UIntPtr.Zero);
  }
}
"@
[W]::SetProcessDPIAware() | Out-Null

function Shot($file) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
  $bmp.Save("$PWD\$file")
}
function State($h) {
  $r = New-Object W+RECT; [W]::GetWindowRect($h, [ref]$r) | Out-Null
  $s = if (-not [W]::IsWindow($h)) { "GONE" } elseif ([W]::IsIconic($h)) { "MINIMIZED" } elseif ([W]::IsZoomed($h)) { "MAXIMIZED" } else { "NORMAL" }
  "$s at $($r.L),$($r.T) size $($r.R - $r.L)x$($r.B - $r.T)"
}
function Rect($h) { $r = New-Object W+RECT; [W]::GetWindowRect($h, [ref]$r) | Out-Null; $r }

$exe = (Resolve-Path target\debug\yard.exe).Path
$env:YARD_HOME = "$env:RUNNER_TEMP\controls-home"
$env:YARD_LIFECYCLE = "info,yard_ui=debug,yard_shell=debug"
$p = Start-Process -FilePath $exe -PassThru -RedirectStandardOutput controls-app.log -RedirectStandardError controls-app-err.log
Start-Sleep 15
$p.Refresh(); $h = $p.MainWindowHandle
Say "window $h dpi $([W]::GetDpiForWindow($h))"
[W]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
[W]::MoveWindow($h, 100, 80, 800, 600, $true) | Out-Null
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep 2
Say "start: $(State $h)"
Shot "controls-normal.png"

# The buttons sit at the bar's right end: 36 px each, the bar 32 px high, at scale 1.
$scale = [W]::GetDpiForWindow($h) / 96.0
function Buttons($h) {
  $c = New-Object W+RECT; [W]::GetClientRect($h, [ref]$c) | Out-Null
  $o = New-Object W+POINT; [W]::ClientToScreen($h, [ref]$o) | Out-Null
  $right = $o.X + $c.R; $y = $o.Y + [int](16 * $scale)
  $closeX = $right - [int](18 * $scale); $maxX = $right - [int](54 * $scale); $minX = $right - [int](90 * $scale); $barX = $o.X + [int](300 * $scale)
  @{ close = @($closeX, $y); max = @($maxX, $y); min = @($minX, $y); bar = @($barX, $y) }
}

$bb = Buttons $h; Say "close at $($bb.close -join ','), max at $($bb.max -join ','), min at $($bb.min -join ',')"

# Hover over close, for a screenshot of the red hover.
$b = Buttons $h; [W]::SetCursorPos($b.close[0], $b.close[1]) | Out-Null; Start-Sleep 1; Shot "controls-hover-close.png"

$b = Buttons $h; [W]::Click($b.max[0], $b.max[1]); Start-Sleep 2; Say "after maximize button: $(State $h)"; Shot "controls-maximized.png"
$b = Buttons $h; [W]::Click($b.max[0], $b.max[1]); Start-Sleep 2; Say "after restore button: $(State $h)"
$b = Buttons $h; [W]::Click($b.bar[0], $b.bar[1]); Start-Sleep -Milliseconds 80; [W]::Click($b.bar[0], $b.bar[1]); Start-Sleep 2; Say "after double-click: $(State $h)"
$b = Buttons $h; [W]::Click($b.bar[0], $b.bar[1]); Start-Sleep -Milliseconds 80; [W]::Click($b.bar[0], $b.bar[1]); Start-Sleep 2; Say "after second double-click: $(State $h)"
$before = State $h; $b = Buttons $h; [W]::Drag($b.bar[0], $b.bar[1], 120, 60); Start-Sleep 2; Say "drag bar: before [$before] after [$(State $h)]"
$before = State $h; $r = Rect $h; [W]::Drag($r.R - 2, $r.T + 300, 80, 0); Start-Sleep 2; Say "drag right edge: before [$before] after [$(State $h)]"
$b = Buttons $h; [W]::RightClick($b.bar[0], $b.bar[1]); Start-Sleep 2; Shot "controls-menu.png"; Say "right-click bar: screenshot controls-menu.png"
[System.Windows.Forms.SendKeys]::SendWait("{ESC}"); Start-Sleep 1
$b = Buttons $h; [W]::Click($b.min[0], $b.min[1]); Start-Sleep 2; Say "after minimize button: $(State $h)"
[W]::ShowWindow($h, 9) | Out-Null; [W]::SetForegroundWindow($h) | Out-Null; Start-Sleep 2; Say "after restore from taskbar: $(State $h)"
$b = Buttons $h; [W]::Click($b.close[0], $b.close[1]); Start-Sleep 4
Say "after close button: process exited=$($p.HasExited) exit=$(if ($p.HasExited) { $p.ExitCode } else { '-' })"
if (-not $p.HasExited) { Stop-Process -Id $p.Id }

# Control: the same drag and right-click on Notepad's system title bar.
Say "== control: Notepad"
$n = Start-Process notepad -PassThru; Start-Sleep 5
$nh = (Get-Process -Id $n.Id).MainWindowHandle
if ($nh -eq 0) { $nh = (Get-Process notepad | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1).MainWindowHandle }
[W]::ShowWindow($nh, 9) | Out-Null; [W]::MoveWindow($nh, 100, 80, 800, 600, $true) | Out-Null; [W]::SetForegroundWindow($nh) | Out-Null; Start-Sleep 2
$r = Rect $nh
$before = State $nh; [W]::Drag($r.L + 300, $r.T + 15, 120, 60); Start-Sleep 2; Say "notepad drag bar: before [$before] after [$(State $nh)]"
$r = Rect $nh; [W]::RightClick($r.L + 300, $r.T + 15); Start-Sleep 2; Shot "control-notepad-menu.png"
[System.Windows.Forms.SendKeys]::SendWait("{ESC}")
Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process

# Control: the smallest GPUI window with a drawn, draggable title bar.
Say "== control: GPUI alone"
$g = Start-Process -FilePath .github\drag_probe\target\debug\drag_probe.exe -PassThru; Start-Sleep 8
$g.Refresh(); $gh = $g.MainWindowHandle
Say "probe window ${gh}: $(State $gh)"
[W]::SetForegroundWindow($gh) | Out-Null; Start-Sleep 1
$c = New-Object W+RECT; [W]::GetClientRect($gh, [ref]$c) | Out-Null
$o = New-Object W+POINT; [W]::ClientToScreen($gh, [ref]$o) | Out-Null
$before = State $gh; [W]::Drag($o.X + 300, $o.Y + 16, 120, 60); Start-Sleep 2; Say "gpui probe drag bar: before [$before] after [$(State $gh)]"
$gx = $o.X + 300; $gy = $o.Y + 16
$o = New-Object W+POINT; [W]::ClientToScreen($gh, [ref]$o) | Out-Null
[W]::Click($o.X + 300, $o.Y + 16); Start-Sleep -Milliseconds 80; [W]::Click($o.X + 300, $o.Y + 16); Start-Sleep 2; Say "gpui probe double-click: $(State $gh)"
Stop-Process -Id $g.Id -ErrorAction SilentlyContinue
