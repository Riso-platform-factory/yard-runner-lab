# Lab: the app on Windows. The .exe's resources, a real run, the directories, and the release subsystem.
$ErrorActionPreference = "Continue"
$out = "windows.txt"
"" | Out-File $out
function Say($text) { $text | Tee-Object -FilePath $out -Append }
Add-Type -AssemblyName System.Windows.Forms, System.Drawing

function Shot($file) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
  $bmp.Save("$PWD\$file")
}

function Subsystem($exe) {
  $bytes = [System.IO.File]::ReadAllBytes($exe)
  $pe = [BitConverter]::ToInt32($bytes, 0x3C)
  $value = [BitConverter]::ToUInt16($bytes, $pe + 0x5C)
  if ($value -eq 2) { "2 (GUI, no console)" } elseif ($value -eq 3) { "3 (console)" } else { "$value" }
}

$exe = (Resolve-Path target\debug\yard.exe).Path
Say "== debug .exe resources"
$v = (Get-Item $exe).VersionInfo
Say "ProductName=$($v.ProductName) FileDescription=$($v.FileDescription) FileVersion=$($v.FileVersion) ProductVersion=$($v.ProductVersion) CompanyName=$($v.CompanyName)"
$icon = [System.Drawing.Icon]::ExtractAssociatedIcon($exe)
$icon.ToBitmap().Save("$PWD\exe-icon.png")
Say "icon extracted: $($icon.Width)x$($icon.Height)"
Say "subsystem: $(Subsystem $exe)"

Say "== run with YARD_HOME"
$env:YARD_LIFECYCLE = "info,yard=debug,yard_paths=debug"
$p = Start-Process -FilePath $exe -PassThru -RedirectStandardOutput app.log -RedirectStandardError app-err.log
Start-Sleep 20
Shot "screenshot.png"
Say "running: $(-not $p.HasExited)"
Get-Process yard -ErrorAction SilentlyContinue | ForEach-Object { Say "window title: $($_.MainWindowTitle)" }
Stop-Process -Id $p.Id -ErrorAction SilentlyContinue
Start-Sleep 2

Say "== run without YARD_HOME: the real directories"
Remove-Item Env:YARD_HOME
$p = Start-Process -FilePath $exe -PassThru -RedirectStandardOutput app-real.log -RedirectStandardError app-real-err.log
Start-Sleep 15
Stop-Process -Id $p.Id -ErrorAction SilentlyContinue
Start-Sleep 2
foreach ($d in "$env:APPDATA\yard", "$env:LOCALAPPDATA\yard") {
  Say "-- $d"
  Get-ChildItem -Recurse $d -ErrorAction SilentlyContinue | ForEach-Object { Say $_.FullName }
}
Select-String -Path app-real.log -Pattern "paths-resolve" | Select-Object -First 6 | ForEach-Object { Say $_.Line }

Say "== release build"
cargo build -p yard --no-default-features --release 2>&1 | Select-Object -Last 2 | ForEach-Object { Say $_ }
$rel = (Resolve-Path target\release\yard.exe).Path
Say "release subsystem: $(Subsystem $rel)"
$p = Start-Process -FilePath $rel -PassThru
Start-Sleep 15
Shot "release.png"
Say "release running: $(-not $p.HasExited)"
Stop-Process -Id $p.Id -ErrorAction SilentlyContinue
