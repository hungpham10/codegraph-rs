$ErrorActionPreference = 'Stop'

$toolsDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$version  = $env:ChocolateyPackageVersion
$url      = "https://github.com/hungpham10/codegraph-rs/releases/download/v$version/codegraph-x86_64-pc-windows-msvc.zip"
$zip      = Join-Path $toolsDir "codegraph-$version.zip"

Get-ChocolateyWebFile -PackageName 'codegraph' -FileFullPath $zip -Url $url
Get-ChocolateyUnzip -FileFullPath $zip -Destination $toolsDir
Remove-Item -Force $zip
