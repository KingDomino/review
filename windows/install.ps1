# Review installer for Windows (per-user; no administrator rights needed).
#
#   install.cmd                 (double-click) or:
#   powershell -ExecutionPolicy Bypass -File install.ps1 [-Uninstall]
param([switch]$Uninstall)
$ErrorActionPreference = 'Stop'

$Dest     = Join-Path $env:LOCALAPPDATA 'Programs\Review'
$Exe      = Join-Path $Dest 'review.exe'
$Classes  = 'HKCU:\Software\Classes'
$ProgId   = 'Review.Document'
$AppKey   = 'HKCU:\Software\Review'
$Uninst   = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Review'
$Shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Review.lnk'
$Exts     = @('pdf', 'xps', 'oxps', 'epub', 'cbz', 'fb2', 'mobi', 'svg', 'png', 'jpg', 'jpeg', 'jpe', 'jfif', 'gif', 'webp', 'tif', 'tiff', 'bmp', 'dib', 'ico', 'cur', 'tga', 'pnm', 'pbm', 'pgm', 'ppm', 'pam', 'dds', 'hdr', 'exr', 'qoi', 'heic', 'heif', 'hif', 'avif', 'jp2', 'j2k', 'jpx', 'jpf', 'psd', 'jxr', 'wdp', 'jxl', 'dng', 'cr2', 'cr3', 'crw', 'nef', 'nrw', 'arw', 'srf', 'sr2', 'orf', 'rw2', 'raf', 'pef', 'srw', 'erf', 'kdc', 'dcr', 'dcs', 'mrw', '3fr', 'iiq', 'mef', 'mos', 'rwl', 'ari', 'gltf', 'glb', 'obj', 'stl', 'usd', 'usda', 'usdc', 'usdz')

function Notify-Shell {
    Add-Type -Namespace Review -Name Shell -MemberDefinition '[DllImport("shell32.dll")] public static extern void SHChangeNotify(int e, uint f, System.IntPtr a, System.IntPtr b);'
    [Review.Shell]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)
}

if ($Uninstall) {
    Write-Host 'Removing Review...'
    Get-Process review -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item $Shortcut -ErrorAction SilentlyContinue
    Remove-Item "$Classes\$ProgId" -Recurse -ErrorAction SilentlyContinue
    Remove-Item "$Classes\Applications\review.exe" -Recurse -ErrorAction SilentlyContinue
    foreach ($e in $Exts) {
        Remove-ItemProperty -Path "$Classes\.$e\OpenWithProgids" -Name $ProgId -ErrorAction SilentlyContinue
    }
    Remove-ItemProperty -Path 'HKCU:\Software\RegisteredApplications' -Name 'Review' -ErrorAction SilentlyContinue
    Remove-Item $AppKey -Recurse -ErrorAction SilentlyContinue
    Remove-Item $Uninst -Recurse -ErrorAction SilentlyContinue
    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($path) {
        $parts = $path.Split(';') | Where-Object { $_ -and ($_ -ne $Dest) }
        [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
    }
    Remove-Item $Dest -Recurse -Force -ErrorAction SilentlyContinue
    Notify-Shell
    Write-Host 'Done. Your settings in %APPDATA%\review were kept.'
    exit 0
}

# ---- pick the binary for this PC -------------------------------------------
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') { 'arm64' } else { 'x64' }
$candidates = @(
    (Join-Path $PSScriptRoot 'review.exe'),
    (Join-Path $PSScriptRoot "..\dist\review-windows-$arch.exe"),
    (Join-Path $PSScriptRoot "review-windows-$arch.exe")
)
$Source = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $Source) { throw "review.exe not found next to the installer (looked for $($candidates -join ', '))." }
Write-Host "Installing Review ($arch) to $Dest"

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Get-Process review -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Copy-Item $Source $Exe -Force
Copy-Item $PSCommandPath (Join-Path $Dest 'install.ps1') -Force

# ---- Start Menu shortcut ----------------------------------------------------
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut($Shortcut)
$lnk.TargetPath = $Exe
$lnk.IconLocation = "$Exe,0"
$lnk.Description = 'View, annotate and edit PDFs and images'
$lnk.Save()

# ---- file types: "Open with" and Default apps --------------------------------
# Create a registry key only if it's missing: New-Item -Force would recreate an existing key
# and wipe values other apps keep there (e.g. their own OpenWithProgids entries).
function Ensure-Key($key) {
    if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
}
function Set-Default($key, $value) {
    Ensure-Key $key
    Set-ItemProperty -Path $key -Name '(default)' -Value $value
}
Set-Default "$Classes\$ProgId" 'Review Document'
Set-Default "$Classes\$ProgId\DefaultIcon" "`"$Exe`",0"
Set-Default "$Classes\$ProgId\shell\open\command" "`"$Exe`" `"%1`""
Set-Default "$Classes\Applications\review.exe\shell\open\command" "`"$Exe`" `"%1`""
Ensure-Key "$Classes\Applications\review.exe\SupportedTypes"
Ensure-Key "$AppKey\Capabilities\FileAssociations"
Set-ItemProperty -Path "$AppKey\Capabilities" -Name 'ApplicationName' -Value 'Review'
Set-ItemProperty -Path "$AppKey\Capabilities" -Name 'ApplicationDescription' -Value 'View, annotate and edit PDFs, images and 3D models'
foreach ($e in $Exts) {
    Ensure-Key "$Classes\.$e\OpenWithProgids"
    New-ItemProperty -Path "$Classes\.$e\OpenWithProgids" -Name $ProgId -Value '' -PropertyType String -Force | Out-Null
    New-ItemProperty -Path "$Classes\Applications\review.exe\SupportedTypes" -Name ".$e" -Value '' -PropertyType String -Force | Out-Null
    Set-ItemProperty -Path "$AppKey\Capabilities\FileAssociations" -Name ".$e" -Value $ProgId
}
Ensure-Key 'HKCU:\Software\RegisteredApplications'
Set-ItemProperty -Path 'HKCU:\Software\RegisteredApplications' -Name 'Review' -Value 'Software\Review\Capabilities'

# ---- Add/Remove Programs entry ----------------------------------------------
Ensure-Key $Uninst
Set-ItemProperty -Path $Uninst -Name 'DisplayName' -Value 'Review'
Set-ItemProperty -Path $Uninst -Name 'DisplayIcon' -Value "$Exe,0"
Set-ItemProperty -Path $Uninst -Name 'Publisher' -Value 'KingDomino'
Set-ItemProperty -Path $Uninst -Name 'DisplayVersion' -Value ((Get-Item $Exe).VersionInfo.ProductVersion)
Set-ItemProperty -Path $Uninst -Name 'InstallLocation' -Value $Dest
Set-ItemProperty -Path $Uninst -Name 'UninstallString' -Value "powershell -NoProfile -ExecutionPolicy Bypass -File `"$Dest\install.ps1`" -Uninstall"
Set-ItemProperty -Path $Uninst -Name 'NoModify' -Value 1 -Type DWord
Set-ItemProperty -Path $Uninst -Name 'NoRepair' -Value 1 -Type DWord

# ---- command line: `review file.pdf` ------------------------------------------
$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $path) { $path = '' }
if (-not ($path.Split(';') -contains $Dest)) {
    [Environment]::SetEnvironmentVariable('Path', (($path.TrimEnd(';') + ';' + $Dest).TrimStart(';')), 'User')
}

Notify-Shell
Write-Host ''
Write-Host 'Installed. Find "Review" in the Start menu, or right-click a file > Open with > Review.'
Write-Host 'To make Review the default for a file type: Settings > Apps > Default apps > Review.'
