# Build MyDeskBridge.exe on Windows.
#   powershell -ExecutionPolicy Bypass -File bridge\build.ps1
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

python -m pip install --upgrade pip
python -m pip install -r requirements.txt pyinstaller

python -m PyInstaller --noconfirm --clean --onefile --name MyDeskBridge `
    --distpath dist --workpath build bridge.py

Write-Host ""
Write-Host "Built: $PSScriptRoot\dist\MyDeskBridge.exe"
