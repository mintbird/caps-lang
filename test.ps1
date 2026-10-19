# Runs the unit tests, then the end-to-end test against the installed app.
#   .\test.ps1          quick run (~20 s)
#   .\test.ps1 -Long    also checks the hook still works after re-arming (+130 s)
#   .\test.ps1 -Unit    unit tests only (no keys are injected)
param([switch]$Long, [switch]$Unit)

Set-Location $PSScriptRoot
Write-Host '== Unit tests ==' -ForegroundColor Cyan
cargo test --release
$unit = $LASTEXITCODE

$e2e = 0
if (-not $Unit) {
    Write-Host '== End-to-end ==' -ForegroundColor Cyan
    & "$PSScriptRoot\tests\e2e.ps1" -Long:$Long
    $e2e = $LASTEXITCODE
}

Write-Host ''
Write-Host ("Unit: {0}" -f $(if ($unit) { 'FAIL' } else { 'PASS' }))
if (-not $Unit) {
    Write-Host ("E2E:  {0}" -f $(switch ($e2e) { 0 { 'PASS' } 2 { 'NOT RUN (see message above)' } default { 'FAIL' } }))
}
exit [int]($unit -or $e2e)
