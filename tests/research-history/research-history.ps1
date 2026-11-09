<#
.SYNOPSIS
  Read-only summary of the web research saved in your Lightagent conversations.

.DESCRIPTION
  Lists every saved web.search, rag.realtime and web.fetch call and classifies
  what the search provider and the page fetches returned. Works with Windows
  PowerShell 5.1 (built into Windows) and PowerShell 7.

  Reads only conversation files: <home>\profiles\<profile>\sessions\<32 hex>.json.
  Never opens config.json, profile.json, .env, secrets, memory, knowledge,
  logs or any other file. Files are opened read-only and shared with other
  programs (read, write and delete sharing), so Lightagent can keep saving
  while the script reads.

  Writes one CSV file in -OutputDir (default: your Desktop), never inside the
  Lightagent home. Changes, moves or deletes nothing. Makes no network
  connection and starts no other program.

  The CSV holds, by default: the time the run was saved (UTC), the period, a
  profile number (P1, P2, ...; not the profile's name), the tool call id, the
  tool, the result category and source counts. Queries and result excerpts are
  added only with -IncludeText, and even then anything that looks like a key,
  token, password, e-mail address or Windows user folder is replaced, and URL
  query strings are removed.

.PARAMETER OldStart
  Start of the period the older build was in use, local time,
  "yyyy-MM-dd HH:mm" (24-hour), e.g. "2026-11-06 19:00". Inclusive.
.PARAMETER OldEnd
  End of that period, local time. Exclusive.
.PARAMETER NewStart
  Start of the period the newer build was in use, local time. Inclusive.
.PARAMETER NewEnd
  End of that period, local time. Exclusive.
.PARAMETER IncludeText
  Also write the (redacted) query and the first 160 characters of the
  (redacted) result to the CSV.
.PARAMETER ShowProfileNames
  Print which profile folder each profile number stands for (console only,
  never in the CSV).

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File .\research-history.ps1 -OldStart "2026-11-06 19:00" -OldEnd "2026-11-07 09:00" -NewStart "2026-11-07 09:30" -NewEnd "2026-11-09 00:00"
#>
[CmdletBinding()]
param(
    [string]$LightagentHome = "",
    [string]$OutputDir = "",
    [string]$OldStart = "", [string]$OldEnd = "",
    [string]$NewStart = "", [string]$NewEnd = "",
    [switch]$IncludeText,
    [switch]$ShowProfileNames
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------- locations
if (-not $LightagentHome) {
    if ($env:LIGHTAGENT_HOME) { $LightagentHome = $env:LIGHTAGENT_HOME }
    else { $LightagentHome = Join-Path $env:USERPROFILE ".lightagent" }
}
if (-not (Test-Path -LiteralPath $LightagentHome -PathType Container)) {
    throw "No Lightagent home at $LightagentHome (pass -LightagentHome)."
}
$LightagentHome = (Resolve-Path -LiteralPath $LightagentHome).Path.TrimEnd('\')
if (-not $OutputDir) { $OutputDir = [Environment]::GetFolderPath("Desktop") }
$OutputDir = [IO.Path]::GetFullPath($OutputDir).TrimEnd('\')
if (($OutputDir + '\').StartsWith($LightagentHome + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to write inside the Lightagent home; choose another -OutputDir."
}
if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
    throw "Output folder $OutputDir does not exist."
}

# ------------------------------------------------------------------ periods
$formats = [string[]]@("yyyy-MM-dd HH:mm", "yyyy-MM-dd HH:mm:ss", "yyyy-MM-dd")
function ToUtc([string]$name, [string]$local) {
    if (-not $local) { return $null }
    $parsed = [DateTime]::MinValue
    $ok = [DateTime]::TryParseExact($local.Trim(), $formats, [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeLocal, [ref]$parsed)
    if (-not $ok) { throw "-$name '$local' is not in the form yyyy-MM-dd HH:mm (24-hour, local time)." }
    return $parsed.ToUniversalTime()
}
$periods = @()
foreach ($p in @(@("old", "OldStart", $OldStart, "OldEnd", $OldEnd), @("new", "NewStart", $NewStart, "NewEnd", $NewEnd))) {
    if (-not $p[2] -and -not $p[4]) { continue }
    if (-not $p[2] -or -not $p[4]) { throw "Give both -$($p[1]) and -$($p[3]), or neither." }
    $start = ToUtc $p[1] $p[2]
    $end = ToUtc $p[3] $p[4]
    if ($end -le $start) { throw "-$($p[3]) must be after -$($p[1])." }
    $periods += , @{ Name = $p[0]; Start = $start; End = $end }
}
if ($periods.Count -eq 2 -and $periods[0].Start -lt $periods[1].End -and $periods[1].Start -lt $periods[0].End) {
    throw "The old and new periods overlap."
}
function PeriodOf($utc) {
    if ($periods.Count -eq 0) { return "all" }
    if ($null -eq $utc) { return "unknown_time" }
    foreach ($p in $periods) {
        if ($utc -ge $p.Start -and $utc -lt $p.End) { return $p.Name }
    }
    return "other"
}

# ----------------------------------------------------------- classification
# Each test matches a fixed text the Lightagent harness writes (3f40cc2):
# crates/lightagent-tools/src/builtins/web.rs, crates/lightagent-rag/src/realtime.rs,
# crates/lightagent-tools/src/executor.rs, crates/lightagent-core/src/loop_.rs,
# crates/lightagent-core/src/permissions.rs, .../identity/revocation.rs.
function Starts([string]$text, [string]$prefix) {
    return $text.StartsWith($prefix, [StringComparison]::Ordinal)
}
function Has([string]$text, [string]$part) {
    return $text.IndexOf($part, [StringComparison]::Ordinal) -ge 0
}
function Classify([string]$tool, [string]$outcome, [string]$text) {
    # Successful results.
    if (Starts $text "CURRENT WEB EVIDENCE") {
        if ($text -match 'Retrieval status: partial \(') { return "evidence_partial" }
        if ($text -match 'Retrieval status: evidence \(') { return "evidence" }
        return "evidence_status_cut"
    }
    if (Starts $text "Results for ") { return "search_results" }
    if ((Starts $text "No current web results found for ") -or (Starts $text "No results for ")) {
        return "provider_zero_results"
    }
    if (Starts $text "No relevant current passages found for ") { return "no_relevant_passage" }
    if ($tool -eq "web.fetch" -and $outcome -eq "ok" -and (Starts $text "URL: ")) { return "page_fetched" }

    # The search provider.
    if ($text -match '^search endpoint returned HTTP (\d{3})') { return "provider_http_" + $Matches[1] }
    if ((Starts $text "search endpoint returned HTML") -or (Starts $text "search endpoint did not return JSON")) {
        return "provider_unexpected_response"
    }
    if (Starts $text "the web search did not answer in time") { return "provider_timeout" }
    # reqwest reports a refused connection, a DNS or TLS failure and its own
    # timeout with the same text, so they cannot be told apart here.
    if (Starts $text "search request failed") { return "provider_request_failed" }
    if (Starts $text "error reading body") {
        if ($tool -eq "web.fetch") { return "page_body_error" }
        return "provider_body_error"
    }
    if (Starts $text "the search returned no usable http(s) source URLs") { return "provider_results_without_urls" }

    # The pages behind the results.
    if (Starts $text "the search returned sources, but none could be read") { return "pages_unreadable" }
    if ($text -match '^\S+ returned HTTP (\d{3})$') { return "page_http_" + $Matches[1] }
    if (Starts $text "could not fetch from ") { return "page_request_failed" }
    if ((Starts $text "refused to fetch from ") -or (Starts $text "refused a redirect from ") -or
        (Starts $text "redirect without a Location header") -or ($text -eq "invalid URL")) {
        return "page_refused"
    }

    # The run, not the provider.
    if ($text -match "^the tool '[^']+' timed out after ") { return "tool_timeout" }
    if (($text -match "^the tool '[^']+' was cancelled") -or (Starts $text "rag.realtime was cancelled")) {
        return "cancelled"
    }
    if (($text -match "^the approval for '[^']+' was denied") -or ($text -match "^the tool '[^']+' requires approval") -or
        ($text -match "^the tool '[^']+' is denied by policy")) {
        return "approval_denied"
    }
    if ((Starts $text "web access is not enabled") -or (Starts $text "no search backend is configured") -or
        (Starts $text "invalid search endpoint") -or (Has $text "not authorized for this profile") -or
        (Starts $text "the search credential") -or (Starts $text "the run's authorization was withdrawn") -or
        ($text -match "^the tool '[^']+' is (not available|no longer permitted)") -or
        (Starts $text "the approval policy is unavailable")) {
        return "configuration_or_authorization"
    }
    if ((Starts $text "could not read ") -or ($text -match "^arguments for '[^']+' are invalid") -or
        (Starts $text "rag.realtime query must not be empty") -or (Starts $text "arguments were not valid JSON")) {
        return "invalid_arguments"
    }
    if ($outcome -eq "error") { return "other_error" }
    return "other_ok"
}

function Counts([string]$tool, [string]$result, [string]$text) {
    $c = [ordered]@{ results = ""; passages = ""; distinct_urls = ""; sources_read = ""; sources_total = "" }
    if ($result -eq "search_results") {
        # render_search: "<n>. <title>" then the URL on the next line.
        $urls = @([regex]::Matches($text, '(?m)^\d+\. .*\r?\n(https?://\S+)') | ForEach-Object { $_.Groups[1].Value })
        $c.results = $urls.Count
        $c.distinct_urls = @($urls | Sort-Object -Unique).Count
    }
    elseif ($result -like "evidence*") {
        $c.passages = ([regex]::Matches($text, '(?m)^\[\d+\] ')).Count
        $urls = @([regex]::Matches($text, '(?m)^URL: (\S+)') | ForEach-Object { $_.Groups[1].Value })
        $c.distinct_urls = @($urls | Sort-Object -Unique).Count
        $m = [regex]::Match($text, 'Retrieval status: \w+ \((\d+) of (\d+) sources read\)')
        if ($m.Success) { $c.sources_read = [int]$m.Groups[1].Value; $c.sources_total = [int]$m.Groups[2].Value }
    }
    elseif ($result -eq "pages_unreadable") {
        $m = [regex]::Match($text, '\((\d+) of (\d+) fetches failed')
        if ($m.Success) {
            $c.sources_total = [int]$m.Groups[2].Value
            $c.sources_read = $c.sources_total - [int]$m.Groups[1].Value
        }
    }
    elseif ($result -eq "provider_zero_results") { $c.results = 0 }
    elseif ($result -eq "page_fetched") { $c.sources_read = 1; $c.sources_total = 1 }
    return $c
}

# ----------------------------------------------------------------- privacy
function Redact([string]$s) {
    if (-not $s) { return "" }
    $s = $s -replace '(https?://[^\s?#"]+)[?#][^\s"]*', '$1'
    $s = $s -replace '(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/\-]+=*', '$1 [redacted]'
    $s = $s -replace '(?i)\b(access[_-]?token|refresh[_-]?token|client[_-]?secret|token|api[_-]?key|key|secret|password|passwd|pwd|authorization|cookie)\b"?\s*[:=]\s*"?[^\s"&,;]+', '$1=[redacted]'
    $s = $s -replace '\b(sk|pk|rk|ghp|gho|ghs|ghu|github_pat|xox[a-z])[-_][A-Za-z0-9_\-]{8,}', '[redacted]'
    $s = $s -replace '\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*', '[redacted]'
    $s = $s -replace '\b(?=[A-Za-z0-9+_\-]*\d)[A-Za-z0-9+_\-]{32,}={0,2}', '[redacted]'
    $s = $s -replace '[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}', '[email]'
    $s = $s -replace '(?i)\b([a-z]:\\users\\)[^\\\s"]+', '$1[user]'
    return ($s -replace '\s+', ' ').Trim()
}

function QueryOf([string]$preview) {
    # The preview is the first 120 characters of the call's JSON arguments.
    if ($preview -match '"(query|url)"\s*:\s*"((?:[^"\\]|\\.)*)') {
        return ($Matches[2] -replace '\\"', '"' -replace '\\\\', '\')
    }
    return ""
}

# -------------------------------------------------------------- reading
$shareAll = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
function ReadShared([string]$path) {
    $stream = New-Object IO.FileStream($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $shareAll)
    try {
        $reader = New-Object IO.StreamReader($stream, (New-Object Text.UTF8Encoding($false)), $true)
        try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}
function HasField($object, [string]$name) {
    return ($null -ne $object) -and ($object.PSObject.Properties.Name -contains $name)
}

$wanted = @("web.search", "rag.realtime", "web.fetch")
$columns = @("saved_utc", "period", "profile", "call", "tool", "result", "outcome", "results", "passages",
    "distinct_urls", "sources_read", "sources_total", "excerpt_truncated", "run_stop")
if ($IncludeText) { $columns += @("query", "excerpt") }

$profiles = [ordered]@{}
$stats = [ordered]@{ files = 0; unreadable = 0; runs = 0; interrupted = 0; runs_without_time = 0 }
$rows = New-Object System.Collections.Generic.List[object]

$profileRoot = Join-Path $LightagentHome "profiles"
$sessionDirs = @()
if (Test-Path -LiteralPath $profileRoot -PathType Container) {
    foreach ($profileDir in @(Get-ChildItem -LiteralPath $profileRoot -Directory | Sort-Object Name)) {
        $dir = Join-Path $profileDir.FullName "sessions"
        if (Test-Path -LiteralPath $dir -PathType Container) { $sessionDirs += , @($profileDir.Name, $dir) }
    }
}

foreach ($entry in $sessionDirs) {
    $profileName = $entry[0]
    if (-not $profiles.Contains($profileName)) { $profiles[$profileName] = "P" + ($profiles.Count + 1) }
    $label = $profiles[$profileName]
    foreach ($file in @(Get-ChildItem -LiteralPath $entry[1] -File -Filter "*.json" | Sort-Object Name)) {
        if ($file.Name -cnotmatch '^[0-9a-f]{32}\.json$') { continue }
        $stats.files++
        try { $session = ConvertFrom-Json (ReadShared $file.FullName) }
        catch { $stats.unreadable++; continue }
        if (-not (HasField $session "runs")) { continue }
        foreach ($run in @($session.runs)) {
            if ($null -eq $run) { continue }
            $stats.runs++
            $stop = ""
            if (HasField $run "stop_reason") { $stop = [string]$run.stop_reason }
            if ($stop -eq "Interrupted") { $stats.interrupted++ }
            $utc = $null
            if ((HasField $run "started_at") -and (HasField $run.started_at "secs_since_epoch")) {
                $utc = [DateTimeOffset]::FromUnixTimeSeconds([int64]$run.started_at.secs_since_epoch).UtcDateTime
            }
            else { $stats.runs_without_time++ }
            if (-not (HasField $run "tools")) { continue }
            foreach ($call in @($run.tools)) {
                if (-not (HasField $call "tool")) { continue }
                $tool = [string]$call.tool
                if ($wanted -notcontains $tool) { continue }
                $text = ""
                if (HasField $call "result_excerpt") { $text = [string]$call.result_excerpt }
                $outcome = ""
                if (HasField $call "outcome") { $outcome = [string]$call.outcome }
                $result = Classify $tool $outcome $text
                $row = [ordered]@{
                    saved_utc = $(if ($utc) { $utc.ToString("yyyy-MM-dd HH:mm:ss", [Globalization.CultureInfo]::InvariantCulture) } else { "" })
                    period    = (PeriodOf $utc)
                    profile   = $label
                    call      = $(if (HasField $call "id") { [string]$call.id } else { "" })
                    tool      = $tool
                    result    = $result
                    outcome   = $outcome
                }
                $counts = Counts $tool $result $text
                foreach ($k in $counts.Keys) { $row[$k] = $counts[$k] }
                $row.excerpt_truncated = $(if ((HasField $call "truncated") -and $call.truncated) { "yes" } else { "no" })
                $row.run_stop = $stop
                if ($IncludeText) {
                    $preview = ""
                    if (HasField $call "arguments_preview") { $preview = [string]$call.arguments_preview }
                    $row.query = Redact (QueryOf $preview)
                    $flat = Redact $text
                    $row.excerpt = $flat.Substring(0, [Math]::Min(160, $flat.Length))
                }
                $rows.Add([pscustomobject]$row)
            }
        }
    }
}

# ---------------------------------------------------------------- output
$sorted = @($rows | Sort-Object saved_utc, call)
$stamp = (Get-Date).ToString("yyyyMMdd-HHmmss", [Globalization.CultureInfo]::InvariantCulture)
$out = Join-Path $OutputDir ("lightagent-research-history-" + $stamp + ".csv")
if (Test-Path -LiteralPath $out) { throw "$out already exists; run again in a second." }
if ($sorted.Count -gt 0) {
    # UTF-8 with a byte-order mark in both editions, so Excel reads accents.
    $encoding = $(if ($PSVersionTable.PSVersion.Major -ge 6) { "utf8BOM" } else { "UTF8" })
    $sorted | Select-Object $columns | Export-Csv -NoTypeInformation -Encoding $encoding -LiteralPath $out
}
else {
    $header = ($columns | ForEach-Object { '"' + $_ + '"' }) -join ','
    [IO.File]::WriteAllText($out, $header + "`r`n", (New-Object Text.UTF8Encoding($true)))
}

Write-Host ("{0} research tool call(s) in {1} conversation file(s), {2} profile(s); written to {3}" -f `
        $sorted.Count, $stats.files, $profiles.Count, $out)
if ($stats.unreadable) { Write-Host "$($stats.unreadable) conversation file(s) could not be read or parsed and were skipped." }
if ($stats.interrupted) { Write-Host "$($stats.interrupted) run(s) were interrupted; Lightagent saves no tool details for those." }
if ($stats.runs_without_time) { Write-Host "$($stats.runs_without_time) run(s) have no saved time (period: unknown_time)." }
if ($ShowProfileNames) {
    Write-Host "Profile numbers (shown here only, never in the CSV):"
    foreach ($k in $profiles.Keys) { Write-Host ("  {0} = {1}" -f $profiles[$k], $k) }
}
if ($sorted.Count -gt 0) {
    Write-Host ""
    Write-Host "Results per period and tool (share = this result's part of that period's calls of that tool):"
    $summary = foreach ($group in @($sorted | Group-Object period, tool)) {
        $total = $group.Count
        foreach ($r in @($group.Group | Group-Object result)) {
            [pscustomobject]@{
                period = $group.Group[0].period; tool = $group.Group[0].tool; result = $r.Name
                calls = $r.Count; of = $total
                share = ([Math]::Round(100.0 * $r.Count / $total)).ToString([Globalization.CultureInfo]::InvariantCulture) + "%"
            }
        }
    }
    $summary | Sort-Object period, tool, @{ Expression = { $_.calls }; Descending = $true }, result |
        Format-Table period, tool, result, calls, of, share -AutoSize | Out-String -Width 200 | Write-Host
}
