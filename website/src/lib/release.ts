// The latest release's downloads, asked of GitHub's API in the browser: a
// new release shows here as soon as it's published, with no rebuild of the
// site. GitHub's "latest" skips drafts and pre-releases, and the release
// workflow publishes a release only once every file is on it.
import { LATEST_RELEASE_API } from '../site';
import type { Arch, Platform, Visitor } from './platform';

export interface Download {
  platform: Platform;
  /** "Apple silicon", "AppImage", "Installer"… */
  label: string;
  /** The file's kind, as shown beside the label: ".dmg", ".deb"… */
  kind: string;
  arch: Arch | 'universal';
  url: string;
  size: number;
}

export interface Release {
  version: string;
  url: string;
  published: string;
  downloads: Download[];
}

export type ReleaseState =
  | { status: 'loading' }
  | { status: 'ready'; release: Release }
  /** Nothing published yet (or the repository can't be seen). */
  | { status: 'none' }
  /** GitHub couldn't be asked: offline, blocked or rate limited. */
  | { status: 'error' };

// The files Tauri's bundler makes, as tauri-action uploads them (a space in
// the product name becomes a dot): "Rovik.IDE_1.2.3_aarch64.dmg",
// "Rovik.IDE_1.2.3_x64-setup.exe", "Rovik.IDE_1.2.3_amd64.AppImage",
// "Rovik.IDE-1.2.3-1.x86_64.rpm". Signatures, updater archives and the
// daemon's builds match none of these. In each platform's order of choice.
const KINDS: { platform: Platform; test: RegExp; label: string; kind: string }[] = [
  { platform: 'macos', test: /\.dmg$/i, label: '', kind: '.dmg' },
  { platform: 'windows', test: /\.exe$/i, label: 'Installer', kind: '.exe' },
  { platform: 'windows', test: /\.msi$/i, label: 'MSI package', kind: '.msi' },
  { platform: 'linux', test: /\.AppImage$/i, label: 'AppImage', kind: 'any distribution' },
  { platform: 'linux', test: /\.deb$/i, label: 'Debian, Ubuntu', kind: '.deb' },
  { platform: 'linux', test: /\.rpm$/i, label: 'Fedora, openSUSE', kind: '.rpm' },
];

interface GitHubAsset {
  name: string;
  browser_download_url: string;
  size: number;
}

function assetArch(name: string): Arch | 'universal' {
  if (/universal/i.test(name)) return 'universal';
  return /aarch64|arm64/i.test(name) ? 'arm64' : 'x64';
}

export function parseRelease(json: unknown): Release | null {
  const r = json as {
    tag_name?: unknown;
    html_url?: unknown;
    published_at?: unknown;
    assets?: GitHubAsset[];
  } | null;
  if (!r || typeof r.tag_name !== 'string' || typeof r.html_url !== 'string') return null;
  const downloads: Download[] = [];
  for (const kind of KINDS) {
    const files = (r.assets ?? [])
      .filter((asset) => kind.test.test(asset.name))
      .map((asset) => ({ asset, arch: assetArch(asset.name) }));
    // Apple silicon before Intel; on other systems the x64 build first.
    const first = kind.platform === 'macos' ? 'arm64' : 'x64';
    files.sort((a, b) => Number(b.arch === first) - Number(a.arch === first));
    for (const { asset, arch } of files) {
      let label = kind.label;
      if (kind.platform === 'macos') {
        label = { arm64: 'Apple silicon', x64: 'Intel', universal: 'Apple silicon and Intel' }[
          arch
        ];
      } else if (files.length > 1) {
        // A format that comes in two architectures says which is which.
        label += arch === 'arm64' ? ' (ARM64)' : ' (x64)';
      }
      downloads.push({
        platform: kind.platform,
        label,
        kind: kind.kind,
        arch,
        url: asset.browser_download_url,
        size: asset.size,
      });
    }
  }
  return {
    version: r.tag_name.replace(/^v/, ''),
    url: r.html_url,
    published: typeof r.published_at === 'string' ? r.published_at : '',
    downloads,
  };
}

const CACHE_KEY = 'rovik-latest-release';
const CACHE_MS = 10 * 60 * 1000;

/**
 * The latest release, kept in localStorage for ten minutes: the API allows
 * an address 60 requests an hour without a token.
 */
export async function loadLatestRelease(): Promise<ReleaseState> {
  try {
    const cached = JSON.parse(localStorage.getItem(CACHE_KEY) ?? 'null') as {
      at: number;
      release: Release;
    } | null;
    if (cached && Date.now() - cached.at < CACHE_MS) {
      return { status: 'ready', release: cached.release };
    }
  } catch {
    // Storage off or unreadable: ask GitHub.
  }

  let response: Response;
  try {
    response = await fetch(LATEST_RELEASE_API, {
      headers: { Accept: 'application/vnd.github+json' },
    });
  } catch {
    return { status: 'error' };
  }
  if (response.status === 404) return { status: 'none' };
  if (!response.ok) return { status: 'error' };

  let release: Release | null;
  try {
    release = parseRelease(await response.json());
  } catch {
    return { status: 'error' };
  }
  if (!release) return { status: 'error' };
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify({ at: Date.now(), release }));
  } catch {
    // Not kept; asked again next time.
  }
  return { status: 'ready', release };
}

/** The download to offer first on the visitor's system. */
export function recommended(release: Release, visitor: Visitor | null): Download | undefined {
  if (!visitor?.platform) return undefined;
  const own = release.downloads.filter((d) => d.platform === visitor.platform);
  // A Mac's browser rarely says which chip it has: Apple silicon then.
  const arch: Arch = visitor.arch ?? (visitor.platform === 'macos' ? 'arm64' : 'x64');
  return own.find((d) => d.arch === arch || d.arch === 'universal') ?? own[0];
}

export function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(0)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} kB`;
}

export function formatDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '';
  return date.toLocaleDateString('en-US', { year: 'numeric', month: 'long', day: 'numeric' });
}
