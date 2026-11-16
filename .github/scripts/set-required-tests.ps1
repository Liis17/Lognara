param(
    [string]$Repository = 'Liis17/Lognara'
)

$ErrorActionPreference = 'Stop'
$files = @(Get-ChildItem (Join-Path $PSScriptRoot '../rulesets') -Filter '*.json' | Sort-Object Name)
$configs = @($files | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json })
# Keep existing rulesets, including MasterBranch, intact.
$rulesets = gh api "repos/$Repository/rulesets" --paginate --slurp | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Cannot read repository rulesets.' }
$rulesets = @($rulesets | ForEach-Object { $_ })
foreach ($ref in ($configs | ForEach-Object { $_.conditions.ref_name.include } | Sort-Object -Unique)) {
    $branch = $ref -replace '^refs/heads/', ''
    # Old workflows also publish CI passed. Require the new guard on both branches.
    $encoded = gh api "repos/$Repository/contents/.github/workflows/ci.yml?ref=$branch" --jq '.content'
    if ($LASTEXITCODE -ne 0) { throw "Cannot read deployed $branch workflow." }
    $workflow = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(($encoded -join '')))
    if ($workflow -notmatch 'name: Promotion source' -or $workflow -notmatch 'python3 .github/scripts/check_ci.py') {
        throw "Deploy the dev -> master workflow to $branch before applying promotion rules. No rules were changed."
    }
    foreach ($script in @('check_ci.py', 'promote_images.py')) {
        gh api "repos/$Repository/contents/.github/scripts/${script}?ref=$branch" --silent
        if ($LASTEXITCODE -ne 0) { throw "Missing $script on $branch. No rules were changed." }
    }
    $checks = gh api "repos/$Repository/commits/$branch/check-runs?per_page=100" --jq '.check_runs[].name'
    if ($LASTEXITCODE -ne 0 -or 'CI passed' -notin $checks) {
        throw "No CI passed check on $branch. No rules were changed."
    }
}
foreach ($file in $files) {
    $config = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
    $existing = @($rulesets | Where-Object { $_.name -eq $config.name })
    if ($existing.Count -gt 1) { throw "Duplicate $($config.name) rulesets." }
    if ($existing.Count -eq 1) {
        gh api --method PUT "repos/$Repository/rulesets/$($existing[0].id)" --input $file.FullName --silent
    } else {
        gh api --method POST "repos/$Repository/rulesets" --input $file.FullName --silent
    }
    if ($LASTEXITCODE -ne 0) { throw "Cannot apply $($config.name)." }
}
