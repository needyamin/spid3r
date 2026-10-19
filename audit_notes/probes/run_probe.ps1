param([string]$Probe, [int]$TimeoutMs = 4000, [string]$Engine = "tree")
$exe = Join-Path $PSScriptRoot "..\..\target\debug\spid3r.exe"
$out = [System.IO.Path]::GetTempFileName()
$err = [System.IO.Path]::GetTempFileName()
$p = Start-Process -FilePath $exe -ArgumentList @("run","--engine",$Engine,$Probe) -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
if (-not $p.WaitForExit($TimeoutMs)) {
    try { $p.Kill($true) } catch { $p.Kill() }
    "TIMEOUT/DEADLOCK (killed after ${TimeoutMs}ms)"
} else {
    "exit=" + $p.ExitCode
}
"--- stdout ---"
Get-Content $out
"--- stderr ---"
Get-Content $err
Remove-Item $out, $err -ErrorAction SilentlyContinue
