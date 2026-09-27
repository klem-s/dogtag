@echo off
cd /d "%~dp0"
if not exist models mkdir models
echo Telechargement des modeles OCR (~12 Mo)...
curl -L https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten -o models\text-detection.rten
curl -L https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten -o models\text-recognition.rten
echo Termine.
pause
