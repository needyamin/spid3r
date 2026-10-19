# F-4 measurement: deep-nesting build at increasing depths, GC unset vs GC=0.
# Tree engine only (both engines share the gc.rs cost centers).
# Every run is killed after 60 s (WaitForExit(60000) + Kill) — never hangs.
# Usage:  powershell -ExecutionPolicy Bypass -File run_fx4_gc.ps1
$ErrorActionPreference = "Stop"
$bin = "C:\Users\needyamin\OneDrive\Desktop\GaliLang\target\debug\spid3r.exe"
$probeDir = "C:\Users\needyamin\OneDrive\Desktop\GaliLang\audit_notes\probes"

# 10k added to show the growth curve cheaply; the audit matrix is 100k..4M.
$depths = @(
    @{ n = 10000;  tag = "10k" },
    @{ n = 100000; tag = "100k" },
    @{ n = 400000; tag = "400k" },
    @{ n = 1000000; tag = "1M" },
    @{ n = 4000000; tag = "4M" }
)

$rows = @()
foreach ($d in $depths) {
    $probe = Join-Path $probeDir ("fx4_build_{0}.matrix" -f $d.tag)
    $src = @"
fn main() {
    let l = list(1);
    let i = 0;
    while i < $($d.n) {
        let n = list();
        n.push(l);
        l = n;
        i = i + 1;
    }
    println("built");
    print("cells=" + string(gc_live_cells()));
    println("");
    return 0;
}
"@
    Set-Content -Path $probe -Value $src -Encoding ASCII

    foreach ($gc in @($null, "0")) {
        if ($null -ne $gc) { $env:SPID3R_GC = $gc } else { Remove-Item env:SPID3R_GC -ErrorAction SilentlyContinue }
        $outF = Join-Path $env:TEMP "fx4_out.txt"
        $errF = Join-Path $env:TEMP "fx4_err.txt"
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $p = Start-Process -FilePath $bin `
            -ArgumentList @("run", "--engine", "tree", $probe) `
            -NoNewWindow -PassThru `
            -RedirectStandardOutput $outF -RedirectStandardError $errF
        $timedout = -not $p.WaitForExit(60000)
        if ($timedout) { $p.Kill() | Out-Null; $p.WaitForExit() | Out-Null }
        $sw.Stop()
        $mode = if ($null -ne $gc) { "GC=0" } else { "GC unset" }
        $status = if ($timedout) { "TIMEOUT >60s (killed)" } else { "exit $($p.ExitCode)" }
        $tail = ((Get-Content $outF -ErrorAction SilentlyContinue) | Select-Object -Last 2) -join " | "
        $errTail = ((Get-Content $errF -ErrorAction SilentlyContinue) | Select-Object -Last 1)
        $rows += [pscustomobject]@{
            depth   = $d.tag
            gc      = $mode
            seconds = [math]::Round($sw.Elapsed.TotalSeconds, 2)
            status  = $status
            output  = "$tail $errTail".Trim()
        }
        Write-Host ("ROW {0} {1} {2}s {3} :: {4}" -f $d.tag, $mode, [math]::Round($sw.Elapsed.TotalSeconds,2), $status, "$tail $errTail".Trim())
    }
}
Remove-Item env:SPID3R_GC -ErrorAction SilentlyContinue
$rows | Format-Table -AutoSize | Out-String -Width 220
Write-Host "DONE fx4 measurement run"
