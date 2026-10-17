import { createMemo } from 'solid-js';
import { PLATFORM_NAMES, type Visitor } from '../lib/platform';
import { formatSize, recommended, type ReleaseState } from '../lib/release';
import { buildFromSourceUrl, LATEST_RELEASE_URL, REPO_URL } from '../site';
import { DownloadIcon, GitHubLogo } from './icons';

interface Cta {
  label: string;
  href: string;
  note: string;
}

const EVERYWHERE = 'For Linux, macOS and Windows';

/** The main button: the visitor's own download once both are known. */
function heroCta(visitor: Visitor | null, state: ReleaseState): Cta {
  const platform = visitor?.platform;
  if (!platform) {
    const note = state.status === 'ready' ? `Version ${state.release.version}` : EVERYWHERE;
    return { label: 'Download', href: '#download', note };
  }
  const label = `Download for ${PLATFORM_NAMES[platform]}`;
  switch (state.status) {
    case 'loading':
      return { label, href: '#download', note: EVERYWHERE };
    case 'none':
      return {
        label: 'Build from source',
        href: buildFromSourceUrl(platform),
        note: 'No release has been published yet',
      };
    case 'error':
      return { label, href: LATEST_RELEASE_URL, note: 'Opens the latest release on GitHub' };
    case 'ready': {
      const download = recommended(state.release, visitor);
      if (!download) {
        return { label: 'Download', href: '#download', note: `Version ${state.release.version}` };
      }
      return {
        label,
        href: download.url,
        note: `Version ${state.release.version} · ${download.label} · ${formatSize(download.size)}`,
      };
    }
  }
}

export default function Hero(props: { visitor: Visitor | null; release: ReleaseState }) {
  const cta = createMemo(() => heroCta(props.visitor, props.release));
  return (
    <section class="hero" id="top">
      <div class="hero-backdrop" aria-hidden="true" />
      <div class="container hero-inner">
        <a class="pill" href="#status">
          <span class="live-dot" aria-hidden="true" />
          In active development
          <span class="pill-sep" aria-hidden="true">
            ·
          </span>
          <span class="pill-langs">
            <span class="pill-long">JavaScript, TypeScript and Rust</span>
            <span class="pill-short">JS, TS and Rust</span>
          </span>
        </a>
        <h1>
          The lightweight IDE with an AI agent that{' '}
          <span class="gradient-text">checks its own work</span>
        </h1>
        <p class="lead">
          Rovik is a native code editor with an agent built in. It plans, edits and runs your code,
          then runs your type check, lint and tests before a change lands. Use any OpenAI-compatible
          model, or one running on your own machine.
        </p>
        <div class="cta-row">
          <a class="button button-primary" href={cta().href}>
            <DownloadIcon size={18} />
            {cta().label}
          </a>
          <a class="button button-ghost" href={REPO_URL}>
            <GitHubLogo size={18} />
            View on GitHub
          </a>
        </div>
        <p class="cta-note">{cta().note}</p>
      </div>
    </section>
  );
}
