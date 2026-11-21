# Lab: one instance and URLs from the command line on Windows (part 5c), end to end.
$ErrorActionPreference = "Continue"
$out = "instance.txt"
"" | Out-File $out
function Say($text) { $text | Tee-Object -FilePath $out -Append }
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateMutexW(IntPtr a, bool o, string n);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
}
"@
$exe = (Resolve-Path target\debug\yard.exe).Path
$env:YARD_LIFECYCLE = "info,yard_shell=debug"

function Home() { $h = Join-Path $env:RUNNER_TEMP ("home-" + [guid]::NewGuid().ToString("N").Substring(0, 8)); $env:YARD_HOME = $h; $h }
function Start-Yard($log, $argList) {
  if ($argList) { Start-Process -FilePath $exe -ArgumentList $argList -PassThru -RedirectStandardOutput "$log.out" -RedirectStandardError $log }
  else { Start-Process -FilePath $exe -PassThru -RedirectStandardOutput "$log.out" -RedirectStandardError $log }
}
function Run-Yard($log, $argList) {
  $p = Start-Yard $log $argList
  if (-not $p.WaitForExit(15000)) { Stop-Process -Id $p.Id; return "timeout" }
  $p.ExitCode
}
function Lines($log, $pattern) { Select-String -Path $log -Pattern $pattern | ForEach-Object { $_.Line.Trim() } }
function Window($p) { $p.Refresh(); $p.MainWindowHandle }

Say "== first start"
Home | Out-Null
$first = Start-Yard "first.log"; Start-Sleep 12
Lines "first.log" "instance-check" | Select-Object -First 3 | ForEach-Object { Say $_ }

Say "== second start with a URL"
Say "second start exit=$(Run-Yard 'second.log' '"yard://nobody/x?token=secret"')"
Lines "second.log" "instance-check" | ForEach-Object { Say $_ }
Start-Sleep 2
Lines "first.log" "instance-request|url-open" | ForEach-Object { Say $_ }
Say "first still running: $(-not $first.HasExited)"
Say "secret in any log: $((Select-String -Path first.log, second.log -Pattern secret -List).Count)"

Say "== minimize, then a third start with no arguments"
$hwnd = Window $first
[W]::ShowWindow($hwnd, 6) | Out-Null; Start-Sleep 2
Say "minimized before: $([W]::IsIconic($hwnd))"
Say "third start exit=$(Run-Yard 'third.log')"
Start-Sleep 3
Say "minimized after: $([W]::IsIconic($hwnd)), foreground is the first: $([W]::GetForegroundWindow() -eq $hwnd)"
Lines "first.log" "received|activation" | Select-Object -Last 2 | ForEach-Object { Say $_ }

Say "== kill the first, then start again"
Stop-Process -Id $first.Id -Force; Start-Sleep 2
$fourth = Start-Yard "fourth.log"; Start-Sleep 12
Say "fourth start running: $(-not $fourth.HasExited)"
Lines "fourth.log" "instance-check" | Select-Object -First 3 | ForEach-Object { Say $_ }

Say "== another home runs its own instance"
$keep = $env:YARD_HOME
Home | Out-Null
$other = Start-Yard "other.log"; Start-Sleep 12
Say "other home running: $(-not $other.HasExited), fourth still running: $(-not $fourth.HasExited)"
Lines "other.log" "instance-check" | Select-Object -First 2 | ForEach-Object { Say $_ }
Stop-Process -Id $other.Id, $fourth.Id -Force -ErrorAction SilentlyContinue; Start-Sleep 2
$env:YARD_HOME = $keep

Say "== two starts at the same moment"
Home | Out-Null
$a = Start-Yard "race1.log"; $b = Start-Yard "race2.log"; Start-Sleep 15
$running = @(); if (-not $a.HasExited) { $running += "A" }; if (-not $b.HasExited) { $running += "B" }
Say "race: running=$($running -join ' ')"
foreach ($p in $a, $b) { if ($p.HasExited) { Say "the other exited with $($p.ExitCode)" } }
Stop-Process -Id $a.Id, $b.Id -Force -ErrorAction SilentlyContinue; Start-Sleep 2

Say "== the mutex held with no pipe: the start gives up after 2 seconds"
Home | Out-Null
$probe = Start-Yard "probe.log"; Start-Sleep 12
$pipe = (Lines "probe.log" "socket=" | Select-Object -First 1) -replace '.*socket=\\\\\.\\pipe\\([^}]*)\}.*', '$1'
Stop-Process -Id $probe.Id -Force; Start-Sleep 2
$name = $pipe -replace '-\d+$', ''
Say "pipe name: $pipe, mutex name: Local\$name"
$m = [W]::CreateMutexW([IntPtr]::Zero, $false, "Local\$name")
$t = [Diagnostics.Stopwatch]::StartNew()
Say "start with the mutex held exit=$(Run-Yard 'held.log' '"yard://nobody/held"') after $([int]$t.Elapsed.TotalMilliseconds) ms"
Get-Content held.log | Select-Object -Last 3 | ForEach-Object { Say $_.Trim() }
[W]::CloseHandle($m) | Out-Null

Say "== release build: second start exit code"
$rel = "target\release\yard.exe"
if (Test-Path $rel) {
  Home | Out-Null
  $r1 = Start-Process -FilePath $rel -PassThru; Start-Sleep 12
  $r2 = Start-Process -FilePath $rel -ArgumentList '"yard://nobody/r"' -PassThru -Wait
  Say "release second start exit=$($r2.ExitCode), first running: $(-not $r1.HasExited)"
  Stop-Process -Id $r1.Id -Force -ErrorAction SilentlyContinue
}

Say "== ERROR lines"
foreach ($f in Get-ChildItem *.log) { Lines $f.Name " ERROR " | ForEach-Object { Say "$($f.Name): $_" } }
