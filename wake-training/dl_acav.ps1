$ErrorActionPreference = "Stop"
$total = 17280000128L
$n = 16
$chunk = [math]::Ceiling($total / $n)
$url = "https://hf-mirror.com/datasets/binhpham/livekit_wakeword_features/resolve/main/openwakeword_features_ACAV100M_2000_hrs_16bit.npy"
$dir = "D:\My things\Learn\高二\Neo\wake-training\data\features\chunks"
New-Item -ItemType Directory -Force -Path $dir | Out-Null

$script = {
    param($url, $out, $start, $end)
    $expect = $end - $start + 1
    for ($r = 0; $r -lt 8; $r++) {
        if ((Test-Path $out) -and ((Get-Item $out).Length -eq $expect)) { return "OK $out (cached)" }
        & curl.exe -sL --fail --connect-timeout 30 --speed-time 60 --speed-limit 10240 -r "${start}-${end}" -o $out $url
        if ($LASTEXITCODE -eq 0 -and (Test-Path $out) -and ((Get-Item $out).Length -eq $expect)) {
            return "OK $out len=$expect"
        }
        Start-Sleep -Seconds (3 * ($r + 1))
    }
    return "FAIL $out"
}

$jobs = @()
for ($i = 0; $i -lt $n; $i++) {
    $s = $i * $chunk
    $e = [math]::Min(($i + 1) * $chunk - 1, $total - 1)
    $o = Join-Path $dir ("part_{0:d2}" -f $i)
    if ((Test-Path $o) -and ((Get-Item $o).Length -eq ($e - $s + 1))) { continue }
    $jobs += Start-Job -ScriptBlock $script -ArgumentList $url, $o, $s, $e
}
Write-Output ("started " + $jobs.Count + " chunk jobs")
if ($jobs.Count -gt 0) {
    $jobs | Wait-Job | Out-Null
    foreach ($j in $jobs) { Receive-Job $j | Write-Output; Remove-Job $j }
}
$bad = @()
for ($i = 0; $i -lt $n; $i++) {
    $s = $i * $chunk
    $e = [math]::Min(($i + 1) * $chunk - 1, $total - 1)
    $o = Join-Path $dir ("part_{0:d2}" -f $i)
    if (-not (Test-Path $o) -or ((Get-Item $o).Length -ne ($e - $s + 1))) { $bad += $i }
}
if ($bad.Count -eq 0) { Write-Output "ALL_CHUNKS_DONE" } else { Write-Output ("MISSING: " + ($bad -join ",")) }
