import importlib.util
import io
import os
from pathlib import Path
import tempfile
import struct
import unittest
import zipfile
from unittest.mock import patch


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


signing = module("sign-android")
building = module("build-android")


def jni_dex(defined):
    descriptor = b"Llivekit/org/jni_zero/JniZero;"
    data = bytearray(152)
    data[:8] = b"dex\n035\0"
    struct.pack_into("<II", data, 0x38, 1, 112)
    struct.pack_into("<II", data, 0x40, 1, 116)
    struct.pack_into("<II", data, 0x60, int(defined), 120)
    struct.pack_into("<I", data, 112, 152)
    data.extend(bytes([len(descriptor)]) + descriptor + b"\0")
    return data


class AndroidReleaseTests(unittest.TestCase):
    def test_notification_permission_must_reach_the_packaged_manifest(self):
        for present in (True, False):
            badging = "uses-permission: name='android.permission.POST_NOTIFICATIONS'" if present else ""
            with patch.object(building.subprocess, 'check_output', return_value=badging):
                if present:
                    building.verify_notification_permission(Path('test.apk'), {'ANDROID_HOME': 'sdk'})
                else:
                    with self.assertRaisesRegex(RuntimeError, 'notification permission'):
                        building.verify_notification_permission(Path('test.apk'), {'ANDROID_HOME': 'sdk'})

    def test_main_activity_must_reuse_the_process_native_runtime(self):
        good = '\n  E: activity (line=1)\n    A: android:name(0x01010003)="dev.dioxus.main.MainActivity"\n    A: android:launchMode(0x0101001d)=(type 0x10)0x2\n'
        for manifest, valid in [(good, True), (good.replace('0x2\n', '0x0\n'), False),
                                (good.replace('dev.dioxus.main.MainActivity', 'other.Activity'), False),
                                (good.replace('    A: android:launchMode(0x0101001d)=(type 0x10)0x2\n', ''), False)]:
            with patch.object(building.subprocess, 'check_output', return_value=manifest):
                if valid:
                    building.verify_activity_mode(Path('test.apk'), {'ANDROID_HOME': 'sdk'})
                else:
                    with self.assertRaisesRegex(RuntimeError, 'singleTask'):
                        building.verify_activity_mode(Path('test.apk'), {'ANDROID_HOME': 'sdk'})

    def test_jni_class_must_be_defined_in_dex_not_only_referenced(self):
        for defined in (False, True):
            data = jni_dex(defined)
            self.assertIn(b"Llivekit/org/jni_zero/JniZero;", data)
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, "w") as archive:
                archive.writestr("classes.dex", data)
            with zipfile.ZipFile(stream) as archive, patch.object(building, "REQUIRED_CLASSES", ("livekit/org/jni_zero/JniZero.class",)):
                if defined:
                    building.verify_java_classes(archive)
                else:
                    with self.assertRaisesRegex(RuntimeError, "JniZero"):
                        building.verify_java_classes(archive)

    def test_invalid_dex_is_rejected(self):
        for data in (b"", b"dex\n035\0", jni_dex(True)[:140]):
            with self.assertRaises(RuntimeError):
                building.dex_classes(data)

    def test_debug_apk_has_a_separate_package_and_visible_label(self):
        good = "package: name='com.discordia.app.debug' versionCode='1'\napplication-debuggable\napplication-label:'Debug Discordia'\n"
        with patch.object(building.subprocess, "check_output", return_value=good):
            building.verify_debug_metadata(Path("debug.apk"), {"ANDROID_HOME": "sdk"})
        for bad in (good.replace("com.discordia.app.debug", "com.discordia.app"), good.replace("Debug Discordia", "Discordia"), good.replace("application-debuggable", "")):
            with patch.object(building.subprocess, "check_output", return_value=bad):
                with self.assertRaises(RuntimeError):
                    building.verify_debug_metadata(Path("debug.apk"), {"ANDROID_HOME": "sdk"})

    def test_release_branding_survives_resource_filename_optimization(self):
        resources = "\n".join(f"resource 0x7f070088 com.discordia.app:drawable/{name}: t=0x03" for name in building.BRANDING)
        with patch.object(building.subprocess, "check_output", return_value=resources):
            building.verify_release_branding(Path("release.apk"), {"ANDROID_HOME": "sdk"})
        with patch.object(building.subprocess, "check_output", return_value=resources.replace("discordia_logo:", "android_logo:")):
            with self.assertRaises(RuntimeError):
                building.verify_release_branding(Path("release.apk"), {"ANDROID_HOME": "sdk"})

    def test_java_artifact_comes_from_the_dioxus_android_profile(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            library = root / "webrtc/lib"
            library.mkdir(parents=True)
            jar = library.parent / "libwebrtc.jar"
            with zipfile.ZipFile(jar, "w") as archive:
                for name in building.REQUIRED_WEBRTC_CLASSES:
                    archive.writestr(name, b"test")
            for profile, cargo_profile in (("release", "android-release"), ("debug", "android-dev")):
                output = root / "aarch64-linux-android" / cargo_profile / "build/webrtc-sys-test/output"
                output.parent.mkdir(parents=True)
                output.write_text(f"cargo:rustc-link-search=native={library}\n", encoding="utf-8")
                self.assertEqual(building.matching_jar(root, "aarch64-linux-android", profile), jar)

    def test_release_metadata_rejects_debug_builds_and_wrong_versions(self):
        env = {"ANDROID_HOME": "sdk", "DISCORDIA_ANDROID_VERSION_CODE": "862", "DISCORDIA_ANDROID_VERSION_NAME": "v0.1.0-pre.862"}
        permission = "uses-permission: name='android.permission.REQUEST_INSTALL_PACKAGES'\n"
        good = "package: name='com.discordia.app' versionCode='862' versionName='v0.1.0-pre.862'\n" + permission
        for bad in (good + "application-debuggable\n", good.replace("'862'", "'1'"), good.replace("com.discordia.app", "other.app"), good.replace("v0.1.0-pre.862", "0.1.0"), good.replace(permission, "")):
            with patch.object(building.subprocess, "check_output", return_value=bad):
                with self.assertRaises(RuntimeError):
                    building.verify_release_metadata(Path("release.apk"), env)
        with patch.object(building.subprocess, "check_output", return_value=good):
            building.verify_release_metadata(Path("release.apk"), env)

    def test_certificate_mismatch_cannot_publish_an_apk(self):
        self.check_signing("b" * 64, succeeds=False)

    def test_aligned_apk_is_published_only_after_certificate_verification(self):
        self.check_signing("a" * 64, succeeds=True)

    def check_signing(self, actual, succeeds):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            certificate = root / "certificate.sha256"
            certificate.write_text("a" * 64, encoding="utf-8")
            output = root / "release.apk"
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if "sign" in command:
                    Path(command[command.index("--out") + 1]).write_bytes(b"signed")

            def verify(command, **kwargs):
                calls.append(command)
                return f"Signer #1 certificate SHA-256 digest: {actual}\n"

            with patch.dict(os.environ, {"ANDROID_KEYSTORE_PASSWORD": "test-store-secret", "ANDROID_KEY_PASSWORD": "test-key-secret"}), patch.object(signing.subprocess, "run", side_effect=run), patch.object(signing.subprocess, "check_output", side_effect=verify):
                if succeeds:
                    signing.sign_apk(root / "input.apk", output, root / "key.jks", "discordia", certificate, root)
                else:
                    with self.assertRaises(ValueError):
                        signing.sign_apk(root / "input.apk", output, root / "key.jks", "discordia", certificate, root)
            self.assertEqual(output.exists(), succeeds)
            self.assertIn("-f", calls[0])
            self.assertIn("sign", calls[1])
            self.assertIn("verify", calls[2])
            self.assertNotIn("test-store-secret", str(calls))
            self.assertNotIn("test-key-secret", str(calls))
            if succeeds:
                self.assertIn("-c", calls[3])


if __name__ == "__main__":
    unittest.main()
