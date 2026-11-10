"use client";

import { DownloadSimple } from "@phosphor-icons/react/ssr";
import { useEffect, useState } from "react";
import { latestRelease, releaseDate, type Release } from "@/lib/release";
import { RELEASES } from "@/lib/site";

type Platform = "mac" | "win" | "android";

type Target = {
  id: string;
  platform: Platform;
  label: string;
  note: string;
  asset: string;
};

const TARGETS: Target[] = [
  { id: "mac", platform: "mac", label: "macOS", note: "Apple silicon, disk image", asset: "Discordia-macos-arm64.dmg" },
  { id: "win", platform: "win", label: "Windows", note: "Installer", asset: "Discordia-windows-setup.exe" },
  { id: "win-portable", platform: "win", label: "Windows portable", note: "Zip, no installer", asset: "Discordia-windows-portable.zip" },
  { id: "android", platform: "android", label: "Android", note: "ARM64 APK", asset: "Discordia-android-arm64.apk" },
];

function detect(): Platform | null {
  const ua = navigator.userAgent;
  if (/Android/i.test(ua)) return "android";
  if (/iPhone|iPad/i.test(ua)) return null;
  if (/Mac/i.test(ua)) return "mac";
  if (/Win/i.test(ua)) return "win";
  return null;
}

function megabytes(n: number): string {
  return `${(n / 1_000_000).toFixed(0)} MB`;
}

export default function Downloads() {
  const [platform, setPlatform] = useState<Platform | null>(null);
  const [release, setRelease] = useState<Release | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setPlatform(detect());
    let live = true;
    latestRelease().then(
      (r) => live && setRelease(r),
      () => live && setFailed(true),
    );
    return () => {
      live = false;
    };
  }, []);

  const assetFor = (name: string) => release?.assets.find((a) => a.name === name);
  const hrefFor = (t: Target) => assetFor(t.asset)?.browser_download_url ?? release?.html_url ?? RELEASES;
  const primary = TARGETS.find((t) => t.platform === platform) ?? null;
  const primaryAsset = primary ? assetFor(primary.asset) : undefined;

  return (
    <div>
      <div className="dl-primary">
        <a className="btn btn-primary btn-lg button-md" href={primary ? hrefFor(primary) : RELEASES}>
          <DownloadSimple size={20} weight="bold" aria-hidden="true" />
          {primary ? `Download for ${primary.label}` : "Download Discordia"}
        </a>
        <p className="dl-version caption-2">
          {release ? (
            <>
              <span className="mono">{release.tag_name}</span>, published {releaseDate(release)}
              {primaryAsset ? `, ${megabytes(primaryAsset.size)}` : ""}. <a href={RELEASES}>All releases</a>
            </>
          ) : failed ? (
            <>
              Builds for macOS, Windows and Android. <a href={RELEASES}>All releases</a>
            </>
          ) : (
            <span className="skeleton" role="status" aria-label="Looking up the latest release" />
          )}
        </p>
      </div>

      <ul className="dl-list">
        {TARGETS.map((t) => {
          const asset = assetFor(t.asset);
          const sig = assetFor(`${t.asset}.minisig`);
          return (
            <li key={t.id} data-current={primary?.id === t.id}>
              <a className="dl-label title-2" href={hrefFor(t)}>
                {t.label}
              </a>
              <span className="dl-note caption-2">
                {t.note}
                {asset ? `, ${megabytes(asset.size)}` : ""}
                {sig ? (
                  <>
                    {" · "}
                    <a href={sig.browser_download_url}>signature</a>
                  </>
                ) : null}
              </span>
            </li>
          );
        })}
      </ul>
      <p className="dl-paused caption-2">
        Linux desktop builds are paused for now. The server still ships for Linux, macOS and Windows on the{" "}
        <a href={RELEASES}>releases page</a>.
      </p>
    </div>
  );
}
