import argparse
import hashlib
import os
import subprocess
import json
import pathlib
import shutil
import urllib.request
import zipfile

parser = argparse.ArgumentParser(description="Prepare local Windows Android build tools for Discordia (Python 3.11+).")
parser.add_argument('--accept-sdk-license', action='store_true', help='Accept https://developer.android.com/studio#terms-and-conditions')
args = parser.parse_args()
if os.name != 'nt':
    parser.error('This setup script is for Windows; use the native Android SDK tools on other hosts.')
if not args.accept_sdk_license:
    parser.error('Read the Android SDK license and pass --accept-sdk-license only if you accept it.')

root = pathlib.Path(__file__).resolve().parents[1] / 'target/mobile-tools'
root.mkdir(parents=True, exist_ok=True)


def read_json(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.load(response)


def download(url, name, digest=None):
    path = root / name
    if not path.exists():
        print(f'Downloading {name}', flush=True)
        partial = path.with_name(name + '.part')
        with urllib.request.urlopen(url, timeout=120) as response, partial.open('wb') as output:
            shutil.copyfileobj(response, output)
        if digest:
            with partial.open('rb') as archive:
                if hashlib.file_digest(archive, 'sha256').hexdigest() != digest:
                    raise RuntimeError(f'Checksum mismatch: {name}')
        partial.replace(path)
    if digest:
        with path.open('rb') as archive:
            if hashlib.file_digest(archive, 'sha256').hexdigest() != digest:
                raise RuntimeError(f'Checksum mismatch: {name}')
    return path


release_url = 'https://github.com/DioxusLabs/dioxus/releases/download/v0.7.10/'
dx_name = 'dx-x86_64-pc-windows-msvc.zip'
with urllib.request.urlopen(release_url + dx_name.replace('.zip', '.sha256')) as response:
    dx_digest = response.read().decode().strip().split()[0]
dx_archive = download(release_url + dx_name, dx_name, dx_digest)
if not (root / 'dioxus/dx.exe').exists():
    with zipfile.ZipFile(dx_archive) as archive:
        archive.extractall(root / 'dioxus')
print('Dioxus CLI extracted', flush=True)

tools_name = 'commandlinetools-win-15859902_latest.zip'
tools_archive = download('https://dl.google.com/android/repository/' + tools_name, tools_name,
                        '90ae805d20434428bffcb699c290860f19bb5f66a67e6b330067e3de801fb04a')
sdk = root / 'android-sdk'
if not (sdk / 'cmdline-tools/latest/bin/sdkmanager.bat').exists():
    with zipfile.ZipFile(tools_archive) as archive:
        archive.extractall(root / 'cmdline-extract')
    (sdk / 'cmdline-tools').mkdir(parents=True, exist_ok=True)
    source = (root / 'cmdline-extract/cmdline-tools').resolve()
    destination = (sdk / 'cmdline-tools/latest').resolve()
    if not source.is_relative_to(root.resolve()) or not destination.is_relative_to(root.resolve()):
        raise RuntimeError('Android tool paths escaped the local tool directory')
    shutil.move(str(source), str(destination))
print('Android command-line tools extracted', flush=True)

jdk_release = read_json('https://api.github.com/repos/adoptium/temurin21-binaries/releases/tags/jdk-21.0.9%2B10')
jdk_asset = next(a for a in jdk_release['assets'] if a['name'].startswith('OpenJDK21U-jdk_x64_windows_hotspot_') and a['name'].endswith('.zip'))
jdk_hash_asset = next(a for a in jdk_release['assets'] if a['name'] == jdk_asset['name'] + '.sha256.txt')
with urllib.request.urlopen(jdk_hash_asset['browser_download_url']) as response:
    jdk_digest = response.read().decode().strip().split()[0]
jdk_archive = download(jdk_asset['browser_download_url'], jdk_asset['name'], jdk_digest)
if not (root / 'java/jdk-21.0.9+10/bin/java.exe').exists():
    with zipfile.ZipFile(jdk_archive) as archive:
        archive.extractall(root / 'java')
print('Java extracted', flush=True)

env = os.environ.copy()
env['JAVA_HOME'] = str(root / 'java/jdk-21.0.9+10')
env['ANDROID_HOME'] = str(sdk)
manager = sdk / 'cmdline-tools/latest/bin/sdkmanager.bat'
command = [str(manager), '--sdk_root=' + str(sdk), 'platform-tools', 'platforms;android-34', 'build-tools;34.0.0', 'ndk;27.2.12479018']
with (root / 'sdk-install.log').open('w', encoding='utf-8') as log:
    result = subprocess.run(command, input='y\n' * 30, text=True, stdout=log, stderr=subprocess.STDOUT, env=env)
if result.returncode:
    raise SystemExit('Android SDK setup failed; inspect target/mobile-tools/sdk-install.log')
print('Android tools ready. In PowerShell: . ./scripts/android-env.ps1', flush=True)
