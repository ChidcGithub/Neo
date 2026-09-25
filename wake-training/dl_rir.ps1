$ErrorActionPreference = "Stop"
# Download MIT RIR wavs from gitee mirror. File-driven sharding (arrays don't survive Start-Job serialization).
$base = "https://gitee.com/hf-datasets/MIT_environmental_impulse_responses/raw/main/16khz"
$outDir = Join-Path $PSScriptRoot "data\rirs\16khz"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# File list: reuse cached rir_files.txt, else extract from saved gitee tree page
$listPath = Join-Path $PSScriptRoot "rir_files.txt"
if (-not (Test-Path $listPath)) {
    $htmlPath = Join-Path $env:TEMP "gitee_tree.html"
    if (-not (Test-Path $htmlPath)) {
        curl.exe -sL --connect-timeout 15 --max-time 90 "https://ai.gitee.com/hf-datasets/davidscripka/MIT_environmental_impulse_responses/tree/main/16khz" -o $htmlPath
    }
    $names = [regex]::Matches((Get-Content $htmlPath -Raw), 'h\d{3}_[A-Za-z0-9_]+\.wav') | ForEach-Object { $_.Value } | Sort-Object -Unique
    [IO.File]::WriteAllLines($listPath, [string[]]$names)
}
$names = [string[]](Get-Content $listPath | Where-Object { $_ -match '\S' } | ForEach-Object { $_.Trim() })
Write-Output "total files: $($names.Count)"

$todo = New-Object System.Collections.Generic.List[string]
foreach ($n in $names) {
    $dst = Join-Path $outDir $n
    if ((Test-Path $dst) -and ((Get-Item $dst).Length -gt 1000)) { continue }
    $todo.Add($n)
}
Write-Output "to download: $($todo.Count)"
$todoPath = Join-Path $PSScriptRoot "rir_todo.txt"
[IO.File]::WriteAllLines($todoPath, $todo.ToArray())

if ($todo.Count -eq 0) {
    Write-Output "in dir: $((Get-ChildItem $outDir -Filter *.wav).Count) / $($names.Count)"
    Write-Output "RIR_DONE"
    exit 0
}

$workers = 8
$jobs = @()
for ($w = 0; $w -lt $workers; $w++) {
    $jobs += Start-Job -ScriptBlock {
        param($todoPath, $base, $outDir, $shard, $nshards)
        $items = [string[]](Get-Content $todoPath | Where-Object { $_ -match '\S' } | ForEach-Object { $_.Trim() })
        $ok = 0; $fail = 0
        for ($i = 0; $i -lt $items.Count; $i++) {
            if ($i % $nshards -ne $shard) { continue }
            $name = $items[$i]
            $dst = Join-Path $outDir $name
            $done = $false
            for ($r = 0; $r -lt 5; $r++) {
                & curl.exe -sL --fail --connect-timeout 15 --max-time 60 -o $dst "$base/$name" 2>$null
                if ($LASTEXITCODE -eq 0 -and (Test-Path $dst) -and ((Get-Item $dst).Length -gt 1000)) { $done = $true; break }
                Start-Sleep -Seconds (2 * ($r + 1))
            }
            if ($done) { $ok++ } else { $fail++; Write-Output "FAIL $name" }
        }
        return "shard $shard done ok=$ok fail=$fail"
    } -ArgumentList $todoPath, $base, $outDir, $w, $workers
}
$jobs | Wait-Job | Out-Null
$jobs | ForEach-Object { Receive-Job $_ }
$jobs | Remove-Job

$got = (Get-ChildItem $outDir -Filter *.wav).Count
Write-Output "in dir: $got / $($names.Count)"
if ($got -ge $names.Count) { Write-Output "RIR_DONE" } else { Write-Output "RIR_INCOMPLETE" }
