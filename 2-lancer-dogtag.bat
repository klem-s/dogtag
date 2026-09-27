@echo off
cd /d "%~dp0"
if not exist config.toml (
  copy config.example.toml config.toml >nul
  echo config.toml cree : ouvre-le, mets ton pseudo a la ligne player, puis relance.
  notepad config.toml
  pause
  exit /b
)
if not exist models\text-detection.rten (
  echo Modeles OCR manquants : lance d abord 1-telecharger-modeles.bat
  pause
  exit /b
)
dogtag.exe %*
pause
