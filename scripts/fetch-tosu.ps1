$ErrorActionPreference = 'Stop'

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$destination = Join-Path $projectRoot 'references\tosu'
$repository = 'https://github.com/tosuapp/tosu.git'

if (-not (Test-Path -LiteralPath (Split-Path -Parent $destination))) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
}

if (Test-Path -LiteralPath (Join-Path $destination '.git')) {
    git -C $destination pull --ff-only
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    $revision = git -C $destination rev-parse --short HEAD
    Write-Output "Tosu source updated in $destination ($revision)"
    exit 0
}

if (Test-Path -LiteralPath $destination) {
    throw "Destination exists without a Git checkout: $destination"
}

git clone --depth 1 --filter=blob:none --sparse $repository $destination
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

git -C $destination sparse-checkout set packages/tosu/src packages/tsprocess/src
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$revision = git -C $destination rev-parse --short HEAD
Write-Output "Tosu source fetched into $destination ($revision)"
