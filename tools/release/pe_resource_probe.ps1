# PE resource-metadata probe (roadmap 6.1 gate: "integrity metadata present on
# every artifact"). Compares the Windows resource metadata (manifest + icon +
# VERSIONINFO, the .rsrc section) of an oracle-built executable against a
# selfhost-built one. Pure file inspection: it only reads the two files; it
# runs nothing and makes no network access.
#
# Usage: powershell -NoProfile -File tools/release/pe_resource_probe.ps1 <oracle.exe> <selfhost.exe>
# Exit:  0 = both carry non-empty, identical resource metadata
#        1 = missing or different (reason printed on stdout)
param(
    [Parameter(Mandatory = $true)][string]$Oracle,
    [Parameter(Mandatory = $true)][string]$SelfHost
)
$ErrorActionPreference = 'Stop'

function Get-RsrcRawSize([string]$Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    if ($b.Length -lt 0x100 -or $b[0] -ne 0x4D -or $b[1] -ne 0x5A) { return -1 }
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    if ($pe + 24 -gt $b.Length) { return -1 }
    $nsec = [BitConverter]::ToUInt16($b, $pe + 6)
    $optsz = [BitConverter]::ToUInt16($b, $pe + 20)
    $sec = $pe + 24 + $optsz
    for ($i = 0; $i -lt $nsec; $i++) {
        $o = $sec + 40 * $i
        if ($o + 40 -gt $b.Length) { break }
        $name = [Text.Encoding]::ASCII.GetString($b, $o, 8).TrimEnd([char]0)
        if ($name -eq '.rsrc') { return [int][BitConverter]::ToUInt32($b, $o + 16) }
    }
    return 0
}

function Get-Probe([string]$Path) {
    $v = (Get-Item -LiteralPath $Path).VersionInfo
    [pscustomobject]@{
        Rsrc        = Get-RsrcRawSize $Path
        Product     = $v.ProductName
        ProductVer  = $v.ProductVersion
        FileVer     = $v.FileVersion
        Description = $v.FileDescription
        Internal    = $v.InternalName
        Original    = $v.OriginalFilename
    }
}

$o = Get-Probe $Oracle
$s = Get-Probe $SelfHost
if ($o.Rsrc -le 0) { Write-Output "ORACLE-NO-METADATA rsrc=$($o.Rsrc)"; exit 1 }
if ($s.Rsrc -le 0) { Write-Output "SELFHOST-NO-METADATA rsrc=$($s.Rsrc)"; exit 1 }
foreach ($f in 'Rsrc', 'Product', 'ProductVer', 'FileVer', 'Description', 'Internal', 'Original') {
    if ($o.$f -ne $s.$f) { Write-Output "DIFF $f oracle=[$($o.$f)] selfhost=[$($s.$f)]"; exit 1 }
}
Write-Output "OK rsrc=$($s.Rsrc)"
exit 0
