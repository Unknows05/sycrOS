# sycrOS installer for Windows 10/11. In PowerShell:
#   irm https://raw.githubusercontent.com/Unknows05/sycrOS/main/install.ps1 | iex
# Downloads the newest release into %LOCALAPPDATA%\sycrOS, adds a Start menu entry
# "sycrOS" (opens with admin rights) and puts `sycros` on your PATH.
$ErrorActionPreference = 'Stop'

$repo = 'Unknows05/sycrOS'
$url  = "https://github.com/$repo/releases/latest/download/sycros-windows-x64.zip"
$dest = Join-Path $env:LOCALAPPDATA 'sycrOS'
$tmp  = Join-Path ([IO.Path]::GetTempPath()) ("sycros-" + [guid]::NewGuid())

Write-Host 'Downloading sycrOS ...'
New-Item -ItemType Directory -Force $tmp | Out-Null
Invoke-WebRequest -Uri $url -OutFile "$tmp\s.zip" -UseBasicParsing
Expand-Archive "$tmp\s.zip" -DestinationPath $tmp -Force

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item "$tmp\sycrOS\*" $dest -Recurse -Force
Remove-Item $tmp -Recurse -Force
Get-ChildItem $dest | Unblock-File

# Start menu entry (runs Start-sycrOS.cmd, which asks for admin rights)
$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'sycrOS.lnk'
$sh  = New-Object -ComObject WScript.Shell
$s   = $sh.CreateShortcut($lnk)
$s.TargetPath = Join-Path $dest 'Start-sycrOS.cmd'
$s.WorkingDirectory = $dest
$s.Description = 'One USB stick for Windows, Linux and macOS installers'
$s.Save()

# `sycros` command in new terminals
$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($path -split ';') -notcontains $dest) {
  [Environment]::SetEnvironmentVariable('Path', ($path.TrimEnd(';') + ';' + $dest).TrimStart(';'), 'User')
}

Write-Host ''
Write-Host ('Installed: ' + (& (Join-Path $dest 'sycros.exe') --version))
Write-Host 'Start it from the Start menu: "sycrOS" (it asks for admin rights).'
