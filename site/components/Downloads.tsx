"use client";

import { useEffect, useState } from "react";

const REPO = "ssotomayor/discordia";
const RELEASES = `https://github.com/${REPO}/releases`;

type Platform = "mac" | "win" | "linux";

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
  { id: "linux", platform: "linux", label: "Linux", note: "x86_64 AppImage", asset: "Discordia-linux-x86_64.AppImage" },
];

type Asset = { name: string; browser_download_url: string; size: number };
type Release = { tag_name: string; published_at: string; html_url: string; assets: Asset[] };

function detect(): Platform | null {
  const ua = navigator.userAgent;
  if (/iPhone|iPad|Android/i.test(ua)) return null;
  if (/Mac/i.test(ua)) return "mac";
  if (/Win/i.test(ua)) return "win";
  if (/Linux|X11/i.test(ua)) return "linux";
  return null;
}

function megabytes(n: number): string {
  return `${(n / 1_000_000).toFixed(0)} MB`;
}

function DownloadIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 3v12m0 0 4-4m-4 4-4-4M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
    </svg>
  );
}

export default function Downloads() {
  const [platform, setPlatform] = useState<Platform | null>(null);
  const [release, setRelease] = useState<Release | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setPlatform(detect());
    const ctl = new AbortController();
    fetch(`https://api.github.com/repos/${REPO}/releases?per_page=1`, {
      signal: ctl.signal,
      headers: { Accept: "application/vnd.github+json" },
    })
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((list: Release[]) => {
        if (list[0]) setRelease(list[0]);
        else setFailed(true);
      })
      .catch(() => {
        if (!ctl.signal.aborted) setFailed(true);
      });
    return () => ctl.abort();
  }, []);

  const assetFor = (name: string) => release?.assets.find((a) => a.name === name);
  const hrefFor = (t: Target) => assetFor(t.asset)?.browser_download_url ?? release?.html_url ?? RELEASES;
  const primary = TARGETS.find((t) => t.platform === platform) ?? null;
  const primaryAsset = primary ? assetFor(primary.asset) : undefined;
  const date = release
    ? new Date(release.published_at).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" })
    : null;

  return (
    <div>
      <div className="dl-primary">
        <a className="btn btn-primary" href={primary ? hrefFor(primary) : RELEASES}>
          <DownloadIcon />
          {primary ? `Download for ${primary.label}` : "Download Discordia"}
        </a>
        <p className="dl-version">
          {release ? (
            <>
              <span className="mono">{release.tag_name}</span>, published {date}
              {primaryAsset ? `, ${megabytes(primaryAsset.size)}` : ""}.{" "}
              <a href={RELEASES}>All releases</a>
            </>
          ) : failed ? (
            <>
              Pre-release builds for macOS, Windows and Linux. <a href={RELEASES}>All releases</a>
            </>
          ) : (
            "Looking up the latest release…"
          )}
        </p>
      </div>

      <ul className="dl-list">
        {TARGETS.map((t) => {
          const asset = assetFor(t.asset);
          const sig = assetFor(`${t.asset}.minisig`);
          return (
            <li key={t.id} data-current={primary?.id === t.id}>
              <a className="dl-label" href={hrefFor(t)}>
                {t.label}
              </a>
              <span className="dl-note">
                {t.note}
                {asset ? `, ${megabytes(asset.size)}` : ""}
                {sig ? (
                  <>
                    {" "}
                    <a href={sig.browser_download_url}>signature</a>
                  </>
                ) : null}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
