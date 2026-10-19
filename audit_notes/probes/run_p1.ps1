$ErrorActionPreference = 'Continue'
$root = 'C:\Users\needyamin\OneDrive\Desktop\GaliLang'
$exe  = Join-Path $root 'target\debug\spid3r.exe'
$pd   = Join-Path $root 'audit_notes\probes'
$outF = Join-Path $pd 'o.txt'
$errF = Join-Path $pd 'e.txt'

function Show($label, $exePath, $argList, $timeoutMs) {
    $p = Start-Process -FilePath $exePath -ArgumentList $argList -NoNewWindow -PassThru `
         -RedirectStandardOutput $outF -RedirectStandardError $errF
    if (-not $p.WaitForExit($timeoutMs)) {
        $p.Kill(); $p.WaitForExit() | Out-Null
        Write-Output ("{0} => TIMEOUT killed after {1}ms" -f $label, $timeoutMs)
    } else {
        $o = (Get-Content $outF -Raw -ErrorAction SilentlyContinue)
        $e = (Get-Content $errF -Raw -ErrorAction SilentlyContinue)
        if ($null -eq $o) { $o = '' }; if ($null -eq $e) { $e = '' }
        $o = ($o -replace "\r?\n", ' | ').Trim()
        $e = ($e -replace "\r?\n", ' | ').Trim()
        if ($o.Length -gt 300) { $o = $o.Substring(0,300) + '...' }
        if ($e.Length -gt 300) { $e = $e.Substring(0,300) + '...' }
        Write-Output ("{0} => exit={1} out=[{2}] err=[{3}]" -f $label, $p.ExitCode, $o, $e)
    }
}

Write-Output '=== P1 candidate 1: list literal arity ==='
foreach ($n in 256, 300) {
    $f = Join-Path $pd ('list{0}.matrix' -f $n)
    Show ("list{0} check" -f $n) $exe @('check', $f) 8000
    foreach ($eng in 'tree','vm') {
        Show ("list{0} run {1}" -f $n, $eng) $exe @('run','--engine',$eng,$f) 8000
    }
    $c = Join-Path $pd ('list{0}.matrixc' -f $n)
    Show ("list{0} compile" -f $n) $exe @('compile', $f, '-o', $c) 8000
}

Write-Output '=== P1 candidate 2: upvalue capture >127 ==='
foreach ($n in 128, 129, 130) {
    $f = Join-Path $pd ('cap{0}.matrix' -f $n)
    foreach ($eng in 'tree','vm') {
        Show ("cap{0} run {1}" -f $n, $eng) $exe @('run','--engine',$eng,$f) 8000
    }
}

Write-Output '=== pending scheduler probes p20-p24 ==='
foreach ($name in 'p20_spawn_deep_recursion','p21_channel_unjoined_exit','p22_mutex_throw','p23_string_repeat_caps','p24_import_spawns') {
    $f = Join-Path $pd ($name + '.matrix')
    foreach ($eng in 'tree','vm') {
        Show ("{0} {1}" -f $name, $eng) $exe @('run','--engine',$eng,$f) 8000
    }
}

Write-Output '=== malformed bytecode probes ==='
$base = Join-Path $pd 'probe_base.matrixc'
Show 'compile hello' $exe @('compile', (Join-Path $root 'examples\hello.matrix'), '-o', $base) 15000
if (Test-Path $base) {
    $b = [System.IO.File]::ReadAllBytes($base)
    Write-Output ("base size={0} first8=[{1}]" -f $b.Length, (($b[0..7] | ForEach-Object { $_.ToString('X2') }) -join ' '))
    # A: truncated to 60%
    $cut = [int]($b.Length * 0.6)
    [System.IO.File]::WriteAllBytes((Join-Path $pd 'mutA_trunc.matrixc'), $b[0..($cut-1)])
    # B: middle byte -> 0xFF
    $bB = [byte[]]$b.Clone(); $mid = [int]($b.Length / 2); $bB[$mid] = 0xFF
    [System.IO.File]::WriteAllBytes((Join-Path $pd 'mutB_ff.matrixc'), $bB)
    # C1: byte 8 (first byte after SPID3R magic + u16 version) -> 0x26 (Return=38)
    $bC1 = [byte[]]$b.Clone(); $bC1[8] = 0x26
    [System.IO.File]::WriteAllBytes((Join-Path $pd 'mutC1_ret8.matrixc'), $bC1)
    # C2: last byte -> 0x26
    $bC2 = [byte[]]$b.Clone(); $bC2[$b.Length-1] = 0x26
    [System.IO.File]::WriteAllBytes((Join-Path $pd 'mutC2_retlast.matrixc'), $bC2)
    foreach ($m in 'mutA_trunc','mutB_ff','mutC1_ret8','mutC2_retlast') {
        Show ($m + ' exec') $exe @('exec', (Join-Path $pd ($m + '.matrixc'))) 8000
    }
} else {
    Write-Output 'probe_base.matrixc was NOT created - compile failed'
}
Write-Output '=== done ==='
