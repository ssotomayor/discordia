"""Package the existing WebRTC Java classes with the dx Android application."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
REQUIRED_WEBRTC_CLASSES = (
    "livekit/org/jni_zero/JniZero.class",
    "livekit/org/jni_zero/CommonApis.class",
    "livekit/org/webrtc/ContextUtils.class",
    "livekit/org/webrtc/HardwareVideoEncoderFactory.class",
    "livekit/org/webrtc/ScreenCapturerAndroid.class",
    "livekit/org/webrtc/Camera2Enumerator.class",
    "livekit/org/webrtc/SurfaceTextureHelper.class",
    "livekit/org/webrtc/SurfaceViewRenderer.class",
)
# Every one of these must reach the final DEX, wherever it came from.
REQUIRED_CLASSES = REQUIRED_WEBRTC_CLASSES + ("com/discordia/app/DioxusForegroundService.class", "com/discordia/app/DioxusPlaybackCapture.class", "com/discordia/app/DioxusDmNotifications.class", "com/discordia/app/DioxusScreenPreview.class")
REQUIRED_SYMBOLS = {
    "Java_com_discordia_app_DioxusForegroundService_onCallAction",
    "Java_livekit_org_webrtc_SoftwareVideoEncoderFactory_nativeCreateFactory",
    "Java_livekit_org_webrtc_SoftwareVideoDecoderFactory_nativeCreateFactory",
    "Java_dev_dioxus_main_MainActivity_initializeWebRtc",
    "Java_dev_dioxus_main_MainActivity_onNetworkChanged",
    "Java_dev_dioxus_main_MainActivity_onVideoFrame",
    "Java_dev_dioxus_main_MainActivity_onCaptureStopped",
    "Java_dev_dioxus_main_MainActivity_shouldCaptureVideoFrame",
    "Java_dev_dioxus_main_MainActivity_onPlaybackAudio",
    "Java_dev_dioxus_main_MainActivity_onPlaybackAudioError",
    "Java_dev_dioxus_main_MainActivity_onDmNotificationOpened",
    "Java_dev_dioxus_main_MainActivity_onDmNotificationVisibility",
    "Java_dev_dioxus_main_MainActivity_onDmNotificationSettingsChanged",
}
BRANDING = ("discordia_logo", "discordia_splash", "discordia_launcher_foreground")


def verify_native_exports(archive, env):
    ndk = Path(env["ANDROID_NDK_HOME"])
    tools = list((ndk / "toolchains/llvm/prebuilt").glob("*/bin/llvm-nm*"))
    tools = [path for path in tools if path.name in ("llvm-nm", "llvm-nm.exe")]
    if len(tools) != 1:
        raise RuntimeError("Cannot locate Android NDK llvm-nm for JNI verification")
    libraries = [name for name in archive.namelist() if name.startswith("lib/") and name.endswith("/libmain.so")]
    if not libraries:
        raise RuntimeError("Final APK is missing libmain.so")
    for name in libraries:
        with tempfile.TemporaryDirectory() as directory:
            library = Path(directory) / "libmain.so"
            with archive.open(name) as source, library.open("wb") as destination:
                shutil.copyfileobj(source, destination)
            output = subprocess.check_output([str(tools[0]), "-D", "--defined-only", str(library)], text=True)
        symbols = {line.split()[-1].split("@")[0] for line in output.splitlines() if line.strip()}
        missing = REQUIRED_SYMBOLS - symbols
        if missing:
            raise RuntimeError(f"{name} is missing JNI exports: {sorted(missing)}")


def dex_classes(data):
    try:
        if len(data) < 112 or not data.startswith(b"dex\n"):
            raise ValueError("invalid header")
        strings = struct.unpack_from("<I", data, 0x3C)[0]
        types = struct.unpack_from("<I", data, 0x44)[0]
        count, definitions = struct.unpack_from("<II", data, 0x60)
        if definitions + count * 32 > len(data):
            raise ValueError("invalid class table")
        classes = set()
        for index in range(count):
            type_index = struct.unpack_from("<I", data, definitions + index * 32)[0]
            string_index = struct.unpack_from("<I", data, types + type_index * 4)[0]
            offset = struct.unpack_from("<I", data, strings + string_index * 4)[0]
            for _ in range(5):
                byte = data[offset]
                offset += 1
                if byte < 128:
                    break
            else:
                raise ValueError("invalid string length")
            end = data.index(b"\0", offset)
            classes.add(data[offset:end])
        return classes
    except (IndexError, ValueError, struct.error) as error:
        raise RuntimeError(f"Invalid APK DEX: {error}") from error


def verify_java_classes(archive):
    classes = set()
    for name in archive.namelist():
        if name.endswith(".dex"):
            classes.update(dex_classes(archive.read(name)))
    for name in REQUIRED_CLASSES:
        descriptor = ("L" + name.removesuffix(".class") + ";").encode()
        if descriptor not in classes:
            raise RuntimeError(f"Final APK is missing {name}")


def matching_jar(target_dir, target, profile):
    candidates = {}
    cargo_profile = "android-release" if profile == "release" else "android-dev"
    for output in (target_dir / target / cargo_profile / "build").glob("webrtc-sys-*/output"):
        for line in output.read_text(encoding="utf-8", errors="replace").splitlines():
            if "rustc-link-search=native=" not in line:
                continue
            library = Path(line.split("rustc-link-search=native=", 1)[1])
            jar = library.parent / "libwebrtc.jar"
            if jar.is_file():
                digest = hashlib.sha256(jar.read_bytes()).hexdigest()
                candidates[digest] = jar
    if len(candidates) != 1:
        raise RuntimeError(f"Expected one matching WebRTC Java artifact, found {len(candidates)}; inspect Cargo build outputs")
    jar = next(iter(candidates.values()))
    with zipfile.ZipFile(jar) as archive:
        missing = set(REQUIRED_WEBRTC_CLASSES) - set(archive.namelist())
        if missing:
            raise RuntimeError(f"WebRTC artifact missing classes: {sorted(missing)}")
    return jar


def run(args, cwd, env):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def verify_release_metadata(apk, env):
    tools = Path(env["ANDROID_HOME"]) / "build-tools/34.0.0"
    aapt = tools / ("aapt.exe" if os.name == "nt" else "aapt")
    badging = subprocess.check_output([str(aapt), "dump", "badging", str(apk)], text=True)
    package = dict(re.findall(r"(\w+)='([^']*)'", badging.splitlines()[0]))
    if package.get("name") != "com.discordia.app" or "application-debuggable" in badging:
        raise RuntimeError("Expected a non-debuggable Discordia release APK")
    if "uses-permission: name='android.permission.REQUEST_INSTALL_PACKAGES'" not in badging:
        raise RuntimeError("Android release APK is missing update installation permission")
    for field, variable in (("versionCode", "DISCORDIA_ANDROID_VERSION_CODE"), ("versionName", "DISCORDIA_ANDROID_VERSION_NAME")):
        if variable in env and package.get(field) != env[variable]:
            raise RuntimeError(f"Android {field} does not match the release version")


def verify_notification_permission(apk, env):
    tools = Path(env["ANDROID_HOME"]) / "build-tools/34.0.0"
    aapt = tools / ("aapt.exe" if os.name == "nt" else "aapt")
    badging = subprocess.check_output([str(aapt), "dump", "badging", str(apk)], text=True)
    if "uses-permission: name='android.permission.POST_NOTIFICATIONS'" not in badging:
        raise RuntimeError("Android APK is missing DM notification permission")


def verify_debug_metadata(apk, env):
    tools = Path(env["ANDROID_HOME"]) / "build-tools/34.0.0"
    aapt = tools / ("aapt.exe" if os.name == "nt" else "aapt")
    badging = subprocess.check_output([str(aapt), "dump", "badging", str(apk)], text=True)
    package = dict(re.findall(r"(\w+)='([^']*)'", badging.splitlines()[0]))
    if package.get("name") != "com.discordia.app.debug" or "application-debuggable" not in badging:
        raise RuntimeError("Expected a separate Discordia debug application")
    if "application-label:'Debug Discordia'" not in badging:
        raise RuntimeError("Debug APK is missing its development label")


def verify_release_branding(apk, env):
    tools = Path(env["ANDROID_HOME"]) / "build-tools/34.0.0"
    aapt = tools / ("aapt.exe" if os.name == "nt" else "aapt")
    resources = subprocess.check_output([str(aapt), "dump", "resources", str(apk)], text=True)
    for name in BRANDING:
        if f"com.discordia.app:drawable/{name}:" not in resources:
            raise RuntimeError(f"Final APK is missing branding resource: {name}")


def verify_activity_mode(apk, env):
    tools = Path(env["ANDROID_HOME"]) / "build-tools/34.0.0"
    aapt = tools / ("aapt.exe" if os.name == "nt" else "aapt")
    manifest = subprocess.check_output([str(aapt), "dump", "xmltree", str(apk), "AndroidManifest.xml"], text=True)
    for activity in re.split(r"\n\s*E: activity \(", manifest)[1:]:
        if re.search(r'android:name[^\n]*="dev\.dioxus\.main\.MainActivity"', activity):
            if re.search(r"android:launchMode[^\n]*=\(type 0x10\)0x2\b", activity):
                return
            break
    raise RuntimeError("Android main activity must use singleTask to preserve the native runtime")


def package_resources(project):
    resources = project / "app/src/main/res"
    for density in ("mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi"):
        (resources / f"mipmap-{density}/ic_launcher.webp").unlink(missing_ok=True)
    shutil.copytree(ROOT / "client/android/res", resources, dirs_exist_ok=True)
    shutil.copytree(ROOT / "client/android/debug/res", project / "app/src/debug/res", dirs_exist_ok=True)
    logo = resources / "drawable-nodpi/discordia_logo.png"
    logo.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "client/assets/icon-1024.png", logo)


def package_sources(project):
    # Tao owns one process-wide native context; a second Activity creation aborts it.
    for variant in ("debug", "release"):
        manifest = project / "app/src" / variant / "AndroidManifest.xml"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "client/android/AndroidManifest.xml", manifest)
    # dx copies only the main activity; the foreground service the manifest
    # declares lives in the app's own package.
    sources = project / "app/src/main/kotlin/com/discordia/app"
    sources.mkdir(parents=True, exist_ok=True)
    shutil.copy2(
        ROOT / "client/android/DioxusForegroundService.kt",
        sources / "DioxusForegroundService.kt",
    )
    shutil.copy2(ROOT / "client/android/DioxusUpdater.kt", sources / "DioxusUpdater.kt")
    shutil.copy2(ROOT / "client/android/DioxusPlaybackCapture.kt", sources / "DioxusPlaybackCapture.kt")
    shutil.copy2(ROOT / "client/android/DioxusDmNotifications.kt", sources / "DioxusDmNotifications.kt")
    shutil.copy2(ROOT / "client/android/DioxusScreenPreview.kt", sources / "DioxusScreenPreview.kt")
    # R8 cannot see method lookups from Rust/JNI or WebRTC's native library.
    shutil.copy2(ROOT / "client/android/discordia.pro", project / "app/discordia.pro")


def install_auto_init(env):
    """dx runs gradle itself, and that run takes no --init-script.

    Gradle auto-applies every `init.d` script in its user home, which is the only
    way the WebRTC classes reach dx's own Kotlin compile.
    """
    home = Path(env.get("GRADLE_USER_HOME") or Path.home() / ".gradle")
    directory = home / "init.d"
    directory.mkdir(parents=True, exist_ok=True)
    installed = directory / "discordia-webrtc.gradle"
    shutil.copy2(ROOT / "client/android/webrtc.dx.init.gradle", installed)
    return installed


def run_quiet(args, cwd, env):
    """The first dx pass may fail where the WebRTC jar is still missing."""
    # Gradle daemons can retain inherited pipes after dx exits, preventing EOF.
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as output:
        result = subprocess.run(args, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT)
        if result.returncode != 0:
            output.seek(0)
            print(output.read()[-8000:])
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--install", action="store_true")
    parser.add_argument("--target", default="aarch64-linux-android")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--version-code", type=int)
    parser.add_argument("--version-name")
    args = parser.parse_args()
    env = dict(os.environ, LIVEKIT_BUNDLE_SKIP="1")
    if args.version_code is not None:
        if not 1 <= args.version_code <= 2_100_000_000:
            parser.error("--version-code must be between 1 and 2100000000")
        env["DISCORDIA_ANDROID_VERSION_CODE"] = str(args.version_code)
    if args.version_name:
        env["DISCORDIA_ANDROID_VERSION_NAME"] = args.version_name
    profile = "release" if args.release else "debug"
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT, env=env
    ))
    target_dir = Path(metadata["target_directory"])
    project = target_dir / "dx" / "Discordia" / profile / "android" / "app"
    if project.is_dir():
        resources = project / "app/src/main/res"
        # dx rewrites its templates but retains our extra resources on incremental builds.
        package_resources(project)
        for density in ("mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi"):
            (resources / f"mipmap-{density}/ic_launcher.png").unlink(missing_ok=True)
    dx = ["dx", "build", "--android", "--target", args.target,
          "--no-default-features", "--features", "mobile", "--bin", "Discordia"]
    if args.release:
        dx.append("--release")
    # dx compiles Kotlin in the same command that builds the Rust, and it only
    # then writes the WebRTC jar this script has to copy in. A first pass over a
    # clean project therefore stops there; the second pass, with the jar and the
    # auto-applied init script in place, is the one that must succeed.
    first = run_quiet(dx, ROOT / "client", env)
    if first.returncode != 0:
        print("First dx pass did not assemble; retrying with the WebRTC jar")
    jar = matching_jar(target_dir, args.target, profile)
    destination = project / "app" / "libs" / "libwebrtc.jar"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(jar, destination)
    auto_init = install_auto_init(env)
    try:
        package_sources(project)
        run(dx, ROOT / "client", env)
    finally:
        auto_init.unlink(missing_ok=True)
    package_resources(project)
    package_sources(project)
    gradle = project / ("gradlew.bat" if os.name == "nt" else "gradlew")
    task = "assembleRelease" if args.release else "assembleDebug"
    run([str(gradle), "--no-daemon", "--init-script", str(ROOT / "client/android/webrtc.init.gradle"), task], project, env)
    apk_dir = project / "app/build/outputs/apk" / profile
    apks = list(apk_dir.glob("*.apk"))
    if len(apks) != 1:
        raise RuntimeError(f"Expected one APK in {apk_dir}, found {len(apks)}")
    apk = apks[0]
    verify_activity_mode(apk, env)
    verify_notification_permission(apk, env)
    if args.release:
        verify_release_metadata(apk, env)
        verify_release_branding(apk, env)
    else:
        verify_debug_metadata(apk, env)
    with zipfile.ZipFile(apk) as archive:
        if not args.release:
            for resource in ("discordia_logo.png", "discordia_splash.xml", "discordia_launcher_foreground.xml"):
                if not any(name.endswith("/" + resource) for name in archive.namelist()):
                    raise RuntimeError(f"Final APK is missing branding resource: {resource}")
        verify_native_exports(archive, env)
        verify_java_classes(archive)
    print(f"Android APK with matching WebRTC classes: {apk}")
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(apk, args.output)
    if args.install:
        if args.release and "unsigned" in apk.name:
            raise RuntimeError("Release APK is unsigned; sign it before installation")
        run(["adb", "install", "-r", str(apk)], ROOT, env)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Android packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
