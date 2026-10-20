import { createMemo, For, Match, Show, Switch, type Component } from 'solid-js';
import { PLATFORM_NAMES, type Platform, type Visitor } from '../lib/platform';
import { formatDate, formatSize, recommended, type ReleaseState } from '../lib/release';
import { buildFromSourceUrl, LATEST_RELEASE_URL, RELEASES_URL } from '../site';
import { AppleLogo, ArrowUpRightIcon, DownloadIcon, LinuxLogo, WindowsLogo } from './icons';

interface PlatformInfo {
  id: Platform;
  logo: Component<{ size?: number }>;
  note: string;
  /** What every release has for it (release.yml), shown before GitHub answers. */
  formats: { label: string; kind: string }[];
}

const PLATFORMS: PlatformInfo[] = [
  {
    id: 'linux',
    logo: LinuxLogo,
    note: '64-bit: AppImage, .deb or .rpm',
    formats: [
      { label: 'AppImage', kind: 'any distribution' },
      { label: 'Debian, Ubuntu', kind: '.deb' },
      { label: 'Fedora, openSUSE', kind: '.rpm' },
    ],
  },
  {
    id: 'macos',
    logo: AppleLogo,
    note: 'Apple silicon and Intel',
    formats: [
      { label: 'Apple silicon', kind: '.dmg' },
      { label: 'Intel', kind: '.dmg' },
    ],
  },
  {
    id: 'windows',
    logo: WindowsLogo,
    note: 'Windows 10 and 11, 64-bit',
    formats: [
      { label: 'Installer', kind: '.exe' },
      { label: 'MSI package', kind: '.msi' },
    ],
  },
];

interface Row {
  label: string;
  meta: string;
  href: string;
}

/** A platform's downloads: the release's files, else links to its page. */
function platformRows(platform: PlatformInfo, state: ReleaseState, visitor: Visitor | null): Row[] {
  if (state.status !== 'ready') {
    return platform.formats.map((f) => ({
      label: f.label,
      meta: f.kind,
      href: LATEST_RELEASE_URL,
    }));
  }
  const own = state.release.downloads.filter((d) => d.platform === platform.id);
  // On the visitor's system, the build that fits it first.
  const best = visitor?.platform === platform.id ? recommended(state.release, visitor) : undefined;
  const ordered = best ? [best, ...own.filter((d) => d !== best)] : own;
  return ordered.map((d) => ({
    label: d.label,
    meta: `${d.kind} · ${formatSize(d.size)}`,
    href: d.url,
  }));
}

function PlatformCard(props: {
  platform: PlatformInfo;
  visitor: Visitor | null;
  release: ReleaseState;
}) {
  const Logo = props.platform.logo;
  const rows = createMemo(() => platformRows(props.platform, props.release, props.visitor));
  const current = () => props.visitor?.platform === props.platform.id;
  return (
    <li class="platform" classList={{ current: current() }}>
      <div class="platform-head">
        <span class="platform-logo">
          <Logo size={24} />
        </span>
        <div>
          <h3>
            {PLATFORM_NAMES[props.platform.id]}
            <Show when={current()}>
              <span class="badge">Your system</span>
            </Show>
          </h3>
          <p>{props.platform.note}</p>
        </div>
      </div>
      <Switch>
        <Match when={props.release.status === 'none'}>
          <p class="platform-empty">No release yet.</p>
          <a class="dl dl-main" href={buildFromSourceUrl(props.platform.id)}>
            <span>Build from source</span>
            <ArrowUpRightIcon size={16} />
          </a>
        </Match>
        <Match when={rows().length === 0}>
          <p class="platform-empty">Not in this release.</p>
          <a class="dl dl-main" href={RELEASES_URL}>
            <span>Other releases</span>
            <ArrowUpRightIcon size={16} />
          </a>
        </Match>
        <Match when={rows().length > 0}>
          <ul class="dl-list">
            <For each={rows()}>
              {(row, i) => (
                <li>
                  <a class="dl" classList={{ 'dl-main': i() === 0 }} href={row.href}>
                    <Show when={i() === 0}>
                      <DownloadIcon size={17} />
                    </Show>
                    <span class="dl-label">{row.label}</span>
                    <span class="dl-meta">{row.meta}</span>
                  </a>
                </li>
              )}
            </For>
          </ul>
        </Match>
      </Switch>
    </li>
  );
}

export default function Downloads(props: { visitor: Visitor | null; release: ReleaseState }) {
  const release = () => (props.release.status === 'ready' ? props.release.release : undefined);
  return (
    <section class="section" id="download">
      <div class="container">
        <header class="section-head section-head-center">
          <p class="eyebrow">Download</p>
          <h2>Get Rovik IDE</h2>
          <p class="section-lead">
            <Switch fallback="The latest release for Linux, macOS and Windows.">
              <Match when={release()}>
                {(r) => (
                  <>
                    Version {r().version}
                    <Show when={formatDate(r().published)}>
                      {(date) => <>, released {date()}</>}
                    </Show>
                    .{' '}
                    <a class="text-link" href={r().url}>
                      Release notes
                      <ArrowUpRightIcon size={15} />
                    </a>
                  </>
                )}
              </Match>
              <Match when={props.release.status === 'none'}>
                No release has been published yet: build Rovik from source for now.
              </Match>
              <Match when={props.release.status === 'error'}>
                GitHub couldn’t be reached, so the links open the latest release’s page.
              </Match>
            </Switch>
          </p>
        </header>
        <ul class="platforms">
          <For each={PLATFORMS}>
            {(platform) => (
              <PlatformCard platform={platform} visitor={props.visitor} release={props.release} />
            )}
          </For>
        </ul>
        <p class="downloads-links">
          <a class="text-link" href={RELEASES_URL}>
            All releases
            <ArrowUpRightIcon size={15} />
          </a>
          <a class="text-link" href={buildFromSourceUrl()}>
            Build from source
            <ArrowUpRightIcon size={15} />
          </a>
        </p>
        <details class="notes">
          <summary>Opening Rovik for the first time</summary>
          <dl>
            <dt>macOS</dt>
            <dd>
              The app isn’t notarized yet. If macOS says it’s damaged or can’t be opened, move it to
              Applications and run:
              <code>xattr -dr com.apple.quarantine "/Applications/Rovik IDE.app"</code>
            </dd>
            <dt>Windows</dt>
            <dd>
              SmartScreen may warn that the publisher is unknown: choose More info, then Run anyway.
            </dd>
            <dt>Linux</dt>
            <dd>
              Make the AppImage executable, then run it:
              <code>chmod +x Rovik.IDE_*.AppImage</code>
            </dd>
            <dt>Virtual files</dt>
            <dd>
              On its first start Rovik may offer to install a virtual-files driver (FUSE or
              macFUSE). It’s optional: it makes isolated chats faster and lets the agent manage its
              own context. On Windows it offers WSL instead, which the agent works in.
            </dd>
          </dl>
        </details>
      </div>
    </section>
  );
}
