# Lab: cargo xtask bundle windows (part 5d), then install, URLs, and uninstall, end to end.
$ErrorActionPreference = "Continue"
$out = "bundle.txt"
"" | Out-File $out
function Say($text) { $text | Tee-Object -FilePath $out -Append }
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
function Shot($file) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
  $bmp.Save("$PWD\$file")
}
Remove-Item Env:YARD_HOME -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force bundle-logs | Out-Null

Say "== bundle"
foreach ($c in 'plain', 'spaced', 'other') {
  cargo xtask bundle windows --config "crates/yard_build_config/tests/configs/$c/product.toml" --out "dist/$c" *> "bundle-logs\bundle-$c.log"
  Say "bundle ${c}: exit=$LASTEXITCODE"
  Get-Content "bundle-logs\bundle-$c.log" | Select-String -Pattern '^sha256|\.exe$|\.zip$|ERROR|WARN|refus' | ForEach-Object { Say "  $($_.Line.Trim())" }
  Get-ChildItem "dist/$c" -ErrorAction SilentlyContinue | ForEach-Object { Say "  $($_.Name) $($_.Length) bytes" }
}
Say "-- zip of plain"
tar.exe -tf (Get-ChildItem dist/plain/*.zip).FullName | ForEach-Object { Say "  $_" }
Say "-- installer version info"
$s = Get-ChildItem dist/plain/*-setup.exe
Say "  $($s.VersionInfo.ProductName) $($s.VersionInfo.ProductVersion)"
Say "== end-to-end test"
cargo test -p xtask --test bundle_windows_end_to_end -- --ignored 2>&1 | Select-String 'test result|FAILED|panicked' | ForEach-Object { Say $_.Line }

function Check($name, $scheme, $id, $var, $dir) {
  Say "== install $name"
  $setup = Get-ChildItem "dist/$dir/*-setup.exe"
  $p = Start-Process $setup.FullName -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$PWD\bundle-logs\install-$dir.log" -Wait -PassThru
  Say "install exit=$($p.ExitCode)"
  $exe = "$env:LOCALAPPDATA\Programs\$name\$name.exe"
  Say "exe: $(Test-Path $exe)"
  $v = (Get-Item $exe -ErrorAction SilentlyContinue).VersionInfo
  Say "exe version info: $($v.ProductName) $($v.ProductVersion)"
  $lnk = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\$name.lnk"
  Say "shortcut: $(Test-Path $lnk)"
  if (Test-Path $lnk) { $sc = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk); Say "shortcut target: $($sc.TargetPath) args=[$($sc.Arguments)]" }
  Say "-- registry"
  $k = "HKCU:\Software\Classes\$scheme"
  Say "  (default)=$((Get-ItemProperty $k -ErrorAction SilentlyContinue).'(default)') URL Protocol present=$($null -ne (Get-ItemProperty $k -ErrorAction SilentlyContinue).'URL Protocol')"
  Say "  DefaultIcon=$((Get-ItemProperty "$k\DefaultIcon" -ErrorAction SilentlyContinue).'(default)')"
  Say "  command=$((Get-ItemProperty "$k\shell\open\command" -ErrorAction SilentlyContinue).'(default)')"
  $uk = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\${id}_is1"
  $u = Get-ItemProperty $uk -ErrorAction SilentlyContinue
  Say "  uninstall: DisplayName=$($u.DisplayName) DisplayVersion=$($u.DisplayVersion) InstallLocation=$($u.InstallLocation)"

  Say "-- URL with the app not running"
  [Environment]::SetEnvironmentVariable($var, 'info,yard_shell=debug', 'Process')
  $trace = "$env:LOCALAPPDATA\$name\logs\lifecycle.jsonl"
  Start-Process "${scheme}://nobody/first"; Start-Sleep 12
  $procs = @(Get-Process -Name $name -ErrorAction SilentlyContinue)
  Say "processes: $($procs.Count), window title: $($procs | Select-Object -First 1 -ExpandProperty MainWindowTitle)"
  Shot "installed-$dir.png"
  Say "trace file: $(Test-Path $trace)"
  Select-String -Path $trace -Pattern 'url-open|urls-handover' -ErrorAction SilentlyContinue | Select-Object -Last 2 | ForEach-Object { Say "  $($_.Line.Substring(0, [Math]::Min(220, $_.Line.Length)))" }

  Say "-- URL with the app running"
  Start-Process "${scheme}://nobody/second"; Start-Sleep 5
  Say "processes: $(@(Get-Process -Name $name -ErrorAction SilentlyContinue).Count)"
  Select-String -Path $trace -Pattern 'instance-request' -ErrorAction SilentlyContinue | Select-Object -Last 2 | ForEach-Object { Say "  $($_.Line.Substring(0, [Math]::Min(220, $_.Line.Length)))" }

  Say "-- install again while the app runs (AppMutex)"
  $p = Start-Process $setup.FullName -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$PWD\bundle-logs\install-running-$dir.log" -Wait -PassThru
  Say "install while running exit=$($p.ExitCode)"
  Select-String -Path "bundle-logs\install-running-$dir.log" -Pattern 'mutex|running|close' -ErrorAction SilentlyContinue | Select-Object -First 3 | ForEach-Object { Say "  $($_.Line.Trim())" }

  Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process; Start-Sleep 2
  Say "-- uninstall"
  if ($u) {
    $cmd = $u.UninstallString.Trim('"')
    Start-Process $cmd -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait
    for ($i = 0; $i -lt 60 -and (Test-Path "$env:LOCALAPPDATA\Programs\$name"); $i++) { Start-Sleep 1 }
  }
  Say "after uninstall: scheme key=$(Test-Path $k) uninstall key=$(Test-Path $uk) app folder=$(Test-Path "$env:LOCALAPPDATA\Programs\$name") shortcut=$(Test-Path $lnk) data kept=$(Test-Path "$env:LOCALAPPDATA\$name")"
  [Environment]::SetEnvironmentVariable($var, $null, 'Process')
}
Check 'Lab' 'yardlab' 'test.yard.lab' 'LAB_LIFECYCLE' 'plain'
Check 'Lab Notes Pro' 'yardlabnotes' 'test.yard.labnotes' 'LAB_NOTES_PRO_LIFECYCLE' 'spaced'
