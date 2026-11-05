@echo off
rem Builds the finished program, then wraps it in dist\DriverKeep-Setup.exe
cargo build --release || exit /b 1
cd installer || exit /b 1
cargo build --release || exit /b 1
cd ..
if not exist dist mkdir dist
copy /y installer\target\release\driverkeep-setup.exe dist\DriverKeep-Setup.exe
copy /y target\release\driverkeep.exe dist\DriverKeep-Portable.exe
certutil -hashfile dist\DriverKeep-Setup.exe SHA256 > dist\DriverKeep-Setup.sha256.txt
