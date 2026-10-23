"""Package the existing WebRTC Java classes with the dx Android application."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
REQUIRED_WEBRTC_CLASSES = (
    "livekit/org/webrtc/ContextUtils.class",
    "livekit/org/webrtc/DefaultVideoEncoderFactory.class",
    "livekit/org/webrtc/ScreenCapturerAndroid.class",
    "livekit/org/webrtc/Camera2Enumerator.class",
    "livekit/org/webrtc/SurfaceTextureHelper.class",
)
# Every one of these must reach the final DEX, wherever it came from.
REQUIRED_CLASSES = REQUIRED_WEBRTC_CLASSES + ("com/discordia/app/DioxusForegroundService.class",)
REQUIRED_SYMBOLS = {
    "Java_livekit_org_webrtc_SoftwareVideoEncoderFactory_nativeCreateFactory",
    "Java_livekit_org_webrtc_SoftwareVideoDecoderFactory_nativeCreateFactory",
    "Java_dev_dioxus_main_MainActivity_initializeWebRtc",
    "Java_dev_dioxus_main_MainActivity_onVideoFrame",
    "Java_dev_dioxus_main_MainActivity_onCaptureStopped",
}


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


def matching_jar(target_dir, target, profile):
    candidates = {}
    for output in (target_dir / target / profile / "build").glob("webrtc-sys-*/output"):
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


def package_resources(project):
    resources = project / "app/src/main/res"
    for density in ("mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi"):
        (resources / f"mipmap-{density}/ic_launcher.webp").unlink(missing_ok=True)
    shutil.copytree(ROOT / "client/android/res", resources, dirs_exist_ok=True)
    logo = resources / "drawable-nodpi/discordia_logo.png"
    logo.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "client/assets/icon-1024.png", logo)


def package_sources(project):
    # dx copies only the main activity; the foreground service the manifest
    # declares lives in the app's own package.
    sources = project / "app/src/main/kotlin/com/discordia/app"
    sources.mkdir(parents=True, exist_ok=True)
    shutil.copy2(
        ROOT / "client/android/DioxusForegroundService.kt",
        sources / "DioxusForegroundService.kt",
    )
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
    args = parser.parse_args()
    env = dict(os.environ, LIVEKIT_BUNDLE_SKIP="1")
    profile = "release" if args.release else "debug"
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT, env=env
    ))
    target_dir = Path(metadata["target_directory"])
    project = target_dir / "dx" / "Discordia" / profile / "android" / "app"
    if project.is_dir():
        resources = project / "app/src/main/res"
        # dx rewrites its templates but retains our extra resources on incremental builds.
        shutil.copy2(ROOT / "client/android/res/values/discordia_theme.xml", resources / "values/discordia_theme.xml")
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
    with zipfile.ZipFile(apk) as archive:
        for resource in ("discordia_logo.png", "discordia_splash.xml", "discordia_launcher_foreground.xml"):
            if not any(name.endswith("/" + resource) for name in archive.namelist()):
                raise RuntimeError(f"Final APK is missing branding resource: {resource}")
        verify_native_exports(archive, env)
        dex = [archive.read(n) for n in archive.namelist() if n.endswith(".dex")]
        for name in REQUIRED_CLASSES:
            if not any(name.removesuffix(".class").encode() in part for part in dex):
                raise RuntimeError(f"Final APK is missing {name}")
    print(f"Android APK with matching WebRTC classes: {apk}")
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
