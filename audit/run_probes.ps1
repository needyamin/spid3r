# audit/run_probes.ps1 - three-way differential runner for audit probes.
# Runs each probe with --engine tree, --engine vm, and (when both succeed)
# compile+exec, compares stdout and exit codes, saves transcripts to audit/tmp.
$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\spid3r.exe"
$probes = Join-Path $root "audit\probes"
$tmp = Join-Path $root "audit\tmp"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

function Invoke-Engine {
    param([string[]]$ArgList, [string]$WorkDir, [int]$TimeoutMs)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.Arguments = ($ArgList | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $psi.WorkingDirectory = $WorkDir
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $outTask = $p.StandardOutput.ReadToEndAsync()
    $errTask = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($TimeoutMs)) {
        try { $p.Kill() } catch {}
        try { $p.WaitForExit(2000) | Out-Null } catch {}
        return @{ Out = ""; Err = "TIMEOUT"; Code = -999 }
    }
    return @{ Out = $outTask.Result; Err = $errTask.Result; Code = $p.ExitCode }
}

$skip = @("probelib.matrix", "cyc_a.matrix", "cyc_b.matrix")
$slow = @{
    "12_vm_stress.matrix"  = 120000
    "15_recursion.matrix"  = 30000
    "22_cyc.matrix"        = 15000
    "13_overflow.matrix"   = 15000
    "13b_biglit.matrix"    = 15000
    "10_conc.matrix"       = 60000
}

$probeFiles = Get-ChildItem $probes -Filter *.matrix |
    Where-Object { $skip -notcontains $_.Name } | Sort-Object Name

$results = @()
foreach ($f in $probeFiles) {
    $timeout = 30000
    if ($slow.ContainsKey($f.Name)) { $timeout = $slow[$f.Name] }

    $t = Invoke-Engine @("run", "--engine", "tree", $f.FullName) $probes $timeout
    $v = Invoke-Engine @("run", "--engine", "vm", $f.FullName) $probes $timeout

    $eCode = $null
    $eOut = $null
    if ($t.Code -ge 0 -and $v.Code -ge 0 -and $t.Code -eq $v.Code -and $t.Out -eq $v.Out) {
        $outFile = Join-Path $tmp ($f.BaseName + ".matrixc")
        $c = Invoke-Engine @("compile", $f.FullName, "-o", $outFile) $probes $timeout
        if ($c.Code -eq 0) {
            $e = Invoke-Engine @("exec", $outFile) $probes $timeout
            $eCode = $e.Code
            $eOut = $e.Out
        } else {
            $eCode = "cfail" + $c.Code
            $eOut = $c.Err
        }
    }

    Set-Content -Path (Join-Path $tmp ($f.BaseName + ".tree.out")) -Encoding UTF8 -Value ("EXIT=" + $t.Code + "`r`n" + $t.Out + "--- stderr ---`r`n" + $t.Err)
    Set-Content -Path (Join-Path $tmp ($f.BaseName + ".vm.out")) -Encoding UTF8 -Value ("EXIT=" + $v.Code + "`r`n" + $v.Out + "--- stderr ---`r`n" + $v.Err)
    if ($null -ne $eOut) {
        Set-Content -Path (Join-Path $tmp ($f.BaseName + ".exec.out")) -Encoding UTF8 -Value ("EXIT=" + $eCode + "`r`n" + $eOut)
    }

    $match2 = ($t.Code -eq $v.Code) -and ($t.Out -eq $v.Out)
    if ($null -eq $eOut) {
        $status = if ($match2) { "MATCH2" } else { "DIFF" }
    } else {
        $status = if ($match2 -and ($t.Out -eq $eOut) -and ($t.Code -eq $eCode)) { "MATCH3" } else { "DIFF" }
    }
    $results += [PSCustomObject]@{
        Probe    = $f.Name
        Status   = $status
        TreeExit = $t.Code
        VmExit   = $v.Code
        ExecExit = $eCode
    }
}
$results | Format-Table -AutoSize | Out-String -Width 160
