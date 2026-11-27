[CmdletBinding()]
param(
    [string]$Binary = "$PSScriptRoot\..\zxs-client\target\x86_64-pc-windows-msvc\release\zxs-cli.exe",
    [string]$Output = "$PSScriptRoot\..\dist\ZXS-S-Windows-x64.msi",
    [string]$Wix = 'wix.exe'
)

$ErrorActionPreference = 'Stop'
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$outputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output)
$wixPath = (Get-Command $Wix -CommandType Application).Source
$manifest = Join-Path $PSScriptRoot 'windows\zxs-client.wxs'
$icon = (Resolve-Path -LiteralPath "$PSScriptRoot\..\zxs-client\apps\cli\windows\zxs-client.ico").Path
$license = (Resolve-Path -LiteralPath "$PSScriptRoot\..\LICENSE").Path
$versionLine = & $binaryPath --version
if ($LASTEXITCODE -ne 0 -or $versionLine -notmatch '^zxs-cli (?<version>\d+\.\d+\.\d+)$') {
    throw 'Could not read the MSI product version from zxs-cli.exe.'
}
$version = $Matches.version

Add-Type -Namespace ZxsInstaller -Name WindowsIcons -MemberDefinition @'
[DllImport("shell32.dll", CharSet = CharSet.Unicode)]
public static extern uint ExtractIconEx(string path, int index, IntPtr large, IntPtr small, uint count);
'@
$iconCount = [ZxsInstaller.WindowsIcons]::ExtractIconEx($binaryPath, -1, [IntPtr]::Zero, [IntPtr]::Zero, 0)
if ($iconCount -eq 0 -or $iconCount -eq [uint32]::MaxValue) {
    throw 'Windows could not find an application icon in zxs-cli.exe.'
}

$stage = Join-Path ([IO.Path]::GetTempPath()) ('zxs-msi-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $stage
Push-Location $stage
try {
    $rtf = Join-Path $stage 'license.rtf'
    $lines = Get-Content -LiteralPath $license | ForEach-Object {
        $_.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
    }
    $body = $lines -join '\par '
    Set-Content -LiteralPath $rtf -Encoding ASCII -Value ('{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\f0\fs20 ' + $body + '}')

    & $wixPath extension add 'WixToolset.UI.wixext/5.0.2'
    if ($LASTEXITCODE -ne 0) { throw 'Could not add the WiX UI extension.' }

    $package = Join-Path $stage 'ZXS-S-Windows-x64.msi'
    $options = @(
        'build', '-arch', 'x64', '-culture', 'zh-CN', '-ext', 'WixToolset.UI.wixext',
        '-d', "Version=$version", '-d', "ZxsBinary=$binaryPath",
        '-d', "LicenseText=$license", '-d', "LicenseRtf=$rtf", '-d', "ZxsIcon=$icon",
        '-o', $package, $manifest
    )
    & $wixPath @options
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the ZXS-S MSI.' }

    $null = [IO.Directory]::CreateDirectory((Split-Path -Parent $outputPath))
    Copy-Item -LiteralPath $package -Destination $outputPath
    Write-Output $outputPath
} finally {
    Pop-Location
    Remove-Item -LiteralPath $stage -Recurse -Force
}
