[CmdletBinding()]
param(
    [string]$SdkRoot = (Join-Path $PSScriptRoot '../target/mobile-tools/android-sdk'),
    [string]$JavaRoot = (Join-Path $PSScriptRoot '../target/mobile-tools/java/jdk-21.0.9+10'),
    [string]$NdkVersion = '27.2.12479018'
)

$mobileNdk = Join-Path $SdkRoot "ndk/$NdkVersion"
$mobileNdkBin = Join-Path $mobileNdk 'toolchains/llvm/prebuilt/windows-x86_64/bin'
foreach ($mobileRequired in @("$SdkRoot/platform-tools/adb.exe", "$JavaRoot/bin/java.exe", "$mobileNdkBin/clang.exe")) {
    if (-not (Test-Path -LiteralPath $mobileRequired)) {
        throw "Missing Android tool: $mobileRequired. Run python scripts/setup-android.py --accept-sdk-license or specify SdkRoot/JavaRoot."
    }
}

$env:ANDROID_HOME = (Resolve-Path -LiteralPath $SdkRoot).Path
$env:ANDROID_SDK_ROOT = $env:ANDROID_HOME
$env:ANDROID_NDK_HOME = (Resolve-Path -LiteralPath $mobileNdk).Path
$env:JAVA_HOME = (Resolve-Path -LiteralPath $JavaRoot).Path
$mobileNdkBin = Join-Path $env:ANDROID_NDK_HOME 'toolchains/llvm/prebuilt/windows-x86_64/bin'
$mobileDx = Join-Path $PSScriptRoot '../target/mobile-tools/dioxus'
$env:PATH = "$mobileNdkBin;$env:JAVA_HOME/bin;$env:ANDROID_HOME/platform-tools;$mobileDx;$env:PATH"
$env:CC_aarch64_linux_android = Join-Path $mobileNdkBin 'aarch64-linux-android26-clang.cmd'
$env:CXX_aarch64_linux_android = Join-Path $mobileNdkBin 'aarch64-linux-android26-clang++.cmd'
$env:AR_aarch64_linux_android = Join-Path $mobileNdkBin 'llvm-ar.exe'
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $env:CC_aarch64_linux_android
