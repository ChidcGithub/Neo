param([switch]$FunctionsOnly)
$ErrorActionPreference = "Stop"

function Test-RirLocalPath([string]$Path) {
    try {
        $current = [IO.Path]::GetFullPath($Path)
        while ($current) {
            try {
                $attributes = [IO.File]::GetAttributes($current)
                if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { return $false }
            } catch [IO.FileNotFoundException] {
            } catch [IO.DirectoryNotFoundException] {
            }
            $current = [IO.Path]::GetDirectoryName($current)
        }
        return $true
    } catch { return $false }
}

function Test-RirWav([string]$Path) {
    if (-not (Test-RirLocalPath $Path) -or -not [IO.File]::Exists($Path)) { return $false }
    $reader = $null
    try {
        $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Path))
        $stream = $reader.BaseStream
        if ($stream.Length -lt 44 -or $stream.Length -gt 67108864) { return $false }
        if ([Text.Encoding]::ASCII.GetString($reader.ReadBytes(4)) -ne 'RIFF') { return $false }
        $size = $reader.ReadUInt32()
        if ([long]$size + 8 -ne $stream.Length) { return $false }
        if ([Text.Encoding]::ASCII.GetString($reader.ReadBytes(4)) -ne 'WAVE') { return $false }
        $fmt = $false; $dataSize = 0L; $align = 0; $fmtSeen = $false; $dataSeen = $false; $chunks = 0
        while ($stream.Position + 8 -le $stream.Length) {
            if (++$chunks -gt 4096) { return $false }
            $id = [Text.Encoding]::ASCII.GetString($reader.ReadBytes(4))
            $length = $reader.ReadUInt32()
            $next = $stream.Position + [long]$length + ($length % 2)
            if ($next -gt $stream.Length) { return $false }
            if ($id -eq 'fmt ') {
                if ($fmtSeen -or $length -lt 16) { return $false }
                $fmtSeen = $true
                $format = $reader.ReadUInt16(); $channels = $reader.ReadUInt16()
                $rate = $reader.ReadUInt32(); $byteRate = $reader.ReadUInt32()
                $align = $reader.ReadUInt16(); $bits = $reader.ReadUInt16()
                $validEncoding = (($format -eq 1) -and ($bits -in @(8, 16, 24, 32))) -or (($format -eq 3) -and ($bits -in @(32, 64)))
                $fmt = $validEncoding -and ($channels -gt 0) -and ($rate -eq 16000) -and ($align -eq $channels * $bits / 8) -and ($byteRate -eq $rate * $align)
            }
            if ($id -eq 'data') {
                # 不接受重复数据块拼接掩盖单块截断，也不接受 fmt 之前的数据。
                if ($dataSeen -or -not $fmt) { return $false }
                $dataSeen = $true
                $dataSize = $length
            }
            $stream.Position = $next
        }
        return $fmt -and ($dataSize -gt 0) -and ($align -gt 0) -and ($dataSize % $align -eq 0) -and ($stream.Position -eq $stream.Length)
    } catch {
        return $false
    } finally {
        if ($null -ne $reader) { $reader.Dispose() }
    }
}

function Save-RirDownload([string]$Url, [string]$Destination, [scriptblock]$Validate) {
    $temporary = "$Destination.$([guid]::NewGuid().ToString('N')).part"
    try {
        if (-not (Test-RirLocalPath $Destination)) { throw 'Unsafe destination path' }
        & curl.exe -sL --fail --connect-timeout 15 --max-time 90 --max-filesize 67108864 -o $temporary $Url 2>$null
        if ($LASTEXITCODE -ne 0) { throw "curl failed: $LASTEXITCODE" }
        if (-not (& $Validate $temporary)) { throw "Downloaded file failed validation" }
        if (-not (Test-RirLocalPath $temporary) -or -not (Test-RirLocalPath $Destination)) { throw 'Path changed during download' }
        if ([IO.File]::Exists($Destination)) {
            [IO.File]::Replace($temporary, $Destination, [System.Management.Automation.Language.NullString]::Value)
        } else {
            [IO.File]::Move($temporary, $Destination)
        }
        return $true
    } catch {
        Write-Warning "Download failed for ${Url}: $_"
        return $false
    } finally {
        # 清理失败不能覆盖验证/发布的返回结果，也不能沿重解析父目录删除外部文件。
        try {
            if ((Test-RirLocalPath $temporary) -and [IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
        } catch { Write-Warning "Unable to remove partial download: $temporary" }
    }
}

if ($FunctionsOnly) { return }

# Download MIT RIR wavs from gitee mirror. File-driven sharding.
$base = "https://gitee.com/hf-datasets/MIT_environmental_impulse_responses/raw/main/16khz"
$outDir = Join-Path $PSScriptRoot "data\rirs\16khz"
if (-not (Test-RirLocalPath $outDir)) { throw 'Unsafe RIR output directory' }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$listPath = Join-Path $PSScriptRoot "rir_files.txt"
if (-not (Test-RirLocalPath $listPath)) { throw 'Unsafe RIR listing path' }
if (-not (Test-Path $listPath)) {
    $htmlPath = Join-Path $env:TEMP "gitee_tree.html"
    $validateTree = { param($path) (Test-RirLocalPath $path) -and ([IO.File]::Exists($path)) -and ([IO.FileInfo]::new($path).Length -le 67108864) -and ([IO.File]::ReadAllText($path) -match 'h\d{3}_[A-Za-z0-9_]+\.wav') }
    if (-not (& $validateTree $htmlPath)) {
        if (-not (Save-RirDownload "https://ai.gitee.com/hf-datasets/davidscripka/MIT_environmental_impulse_responses/tree/main/16khz" $htmlPath $validateTree)) {
            throw "Unable to download a valid RIR listing"
        }
    }
    $names = [regex]::Matches((Get-Content $htmlPath -Raw), 'h\d{3}_[A-Za-z0-9_]+\.wav') | ForEach-Object { $_.Value } | Sort-Object -Unique
    [IO.File]::WriteAllLines($listPath, [string[]]$names)
}
$names = [string[]](Get-Content $listPath | Where-Object { $_ -match '\S' } | ForEach-Object { $_.Trim() } | Sort-Object -Unique)
if ($names.Count -eq 0 -or @($names | Where-Object { $_ -notmatch '^h\d{3}_[A-Za-z0-9_]+\.wav$' }).Count -gt 0) {
    throw "Invalid or empty cached RIR listing: $listPath"
}
Write-Output "total files: $($names.Count)"
$todo = New-Object System.Collections.Generic.List[string]
foreach ($n in $names) {
    $dst = Join-Path $outDir $n
    if (Test-RirWav $dst) { continue }
    $todo.Add($n)
}
Write-Output "to download: $($todo.Count)"
$todoPath = Join-Path $PSScriptRoot "rir_todo.txt"
if (-not (Test-RirLocalPath $todoPath)) { throw 'Unsafe RIR todo path' }
[IO.File]::WriteAllLines($todoPath, $todo.ToArray())
if ($todo.Count -eq 0) {
    Write-Output "RIR_DONE"
    exit 0
}

$workers = 8
$jobs = @()
for ($w = 0; $w -lt $workers; $w++) {
    $jobs += Start-Job -ScriptBlock {
        param($todoPath, $base, $outDir, $shard, $nshards, $scriptPath)
        . $scriptPath -FunctionsOnly
        $items = [string[]](Get-Content $todoPath | Where-Object { $_ -match '\S' } | ForEach-Object { $_.Trim() })
        $ok = 0; $fail = 0
        for ($i = 0; $i -lt $items.Count; $i++) {
            if ($i % $nshards -ne $shard) { continue }
            $name = $items[$i]
            $dst = Join-Path $outDir $name
            $done = $false
            for ($r = 0; $r -lt 5; $r++) {
                if (Save-RirDownload "$base/$name" $dst { param($path) Test-RirWav $path }) { $done = $true; break }
                Start-Sleep -Seconds (2 * ($r + 1))
            }
            if ($done) { $ok++ } else { $fail++; Write-Output "FAIL $name" }
        }
        return "shard $shard done ok=$ok fail=$fail"
    } -ArgumentList $todoPath, $base, $outDir, $w, $workers, $PSCommandPath
}
$jobs | Wait-Job | Out-Null
$jobs | ForEach-Object { Receive-Job $_ }
$jobs | Remove-Job
$got = @($names | Where-Object { Test-RirWav (Join-Path $outDir $_) }).Count
Write-Output "in dir: $got / $($names.Count)"
if ($got -eq $names.Count) { Write-Output "RIR_DONE" } else { Write-Output "RIR_INCOMPLETE"; exit 1 }
