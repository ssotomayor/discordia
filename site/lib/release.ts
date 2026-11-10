import { REPO } from "./site";

export type Asset = { name: string; browser_download_url: string; size: number };
export type Release = { tag_name: string; published_at: string; html_url: string; assets: Asset[] };

let pending: Promise<Release> | null = null;

// Hero and download section share one request: the unauthenticated API allows 60 an hour per address.
export function latestRelease(): Promise<Release> {
  pending ??= fetch(`https://api.github.com/repos/${REPO}/releases?per_page=1`, {
    headers: { Accept: "application/vnd.github+json" },
  })
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
    .then((list: Release[]) => list[0] ?? Promise.reject(new Error("no releases")));
  return pending;
}

export function releaseDate(r: Release): string {
  return new Date(r.published_at).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}
