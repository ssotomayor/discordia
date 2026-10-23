"""Align, sign and verify an APK using an existing Android release key."""
import argparse
from pathlib import Path
import os
import re
import subprocess
import tempfile


def fingerprint(text):
    value = text.strip().replace(":", "").lower()
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("Expected one SHA-256 signing certificate fingerprint")
    return value


def sign_apk(source, output, keystore, alias, certificate, build_tools):
    expected = fingerprint(certificate.read_text(encoding="utf-8"))
    for name in ("ANDROID_KEYSTORE_PASSWORD", "ANDROID_KEY_PASSWORD"):
        if not os.environ.get(name):
            raise ValueError(f"Missing {name}")
    zipalign = build_tools / ("zipalign.exe" if os.name == "nt" else "zipalign")
    apksigner = build_tools / ("apksigner.bat" if os.name == "nt" else "apksigner")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent) as directory:
        aligned = Path(directory) / "aligned.apk"
        signed = Path(directory) / "signed.apk"
        subprocess.run([str(zipalign), "-f", "-p", "4", str(source), str(aligned)], check=True)
        subprocess.run([
            str(apksigner), "sign", "--ks", str(keystore), "--ks-key-alias", alias,
            "--ks-pass", "env:ANDROID_KEYSTORE_PASSWORD", "--key-pass", "env:ANDROID_KEY_PASSWORD",
            "--v4-signing-enabled", "false", "--out", str(signed), str(aligned),
        ], check=True)
        verified = subprocess.check_output([
            str(apksigner), "verify", "--verbose", "--print-certs", str(signed),
        ], text=True)
        certificates = re.findall(r"^Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F:]+)$", verified, re.MULTILINE)
        if len(certificates) != 1 or fingerprint(certificates[0]) != expected:
            raise ValueError("APK signing certificate differs from the committed release certificate")
        subprocess.run([str(zipalign), "-c", "-p", "4", str(signed)], check=True)
        signed.replace(output)
    print(f"Verified signed Android APK: {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--keystore", required=True, type=Path)
    parser.add_argument("--alias", default="discordia")
    parser.add_argument("--certificate", type=Path, default=Path(__file__).resolve().parents[1] / "client/android/release-signing.sha256")
    parser.add_argument("--build-tools", type=Path, required=True)
    args = parser.parse_args()
    sign_apk(args.input, args.output, args.keystore, args.alias, args.certificate, args.build_tools)


if __name__ == "__main__":
    main()
