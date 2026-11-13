@echo off
rem Builds dist\DriverKeep.msi from target\release\driverkeep.exe using WiX Toolset v3.
rem Needs WiX v3 (e.g. "choco install wixtoolset", which sets the WIX variable) or the portable
rem wix314-binaries.zip unpacked somewhere, with the WIX variable pointing at that folder.
rem Does not change make-installer.cmd or the DriverKeep-Setup.exe installer.
setlocal
if "%WIX%"=="" (
  echo WiX Toolset v3 not found. Install it first, for example: choco install wixtoolset
  exit /b 1
)
rem Installed WiX keeps its tools in %WIX%bin; the portable zip has them directly in %WIX%.
set "WIXBIN=%WIX%bin\"
if exist "%WIX%candle.exe" set "WIXBIN=%WIX%"
cargo build --release || exit /b 1
set VER=
for /f "tokens=3 delims= " %%v in ('findstr /b /c:"version = " Cargo.toml') do if not defined VER set VER=%%~v
if "%VER%"=="" ( echo Could not read version from Cargo.toml & exit /b 1 )
if not exist dist mkdir dist
if not exist installer-msi\obj mkdir installer-msi\obj
"%WIXBIN%candle.exe" -nologo -arch x64 -dVersion=%VER% -dExePath=target\release\driverkeep.exe -out installer-msi\obj\ installer-msi\DriverKeep.wxs || exit /b 1
"%WIXBIN%light.exe" -nologo -out dist\DriverKeep.msi installer-msi\obj\DriverKeep.wixobj || exit /b 1
certutil -hashfile dist\DriverKeep.msi SHA256 > dist\DriverKeep.msi.sha256.txt
echo Built dist\DriverKeep.msi (version %VER%)
