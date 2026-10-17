import { For, type Component } from 'solid-js';
import { ISSUES_URL } from '../site';
import { ArrowRightIcon, JavaScriptLogo, RustLogo, TypeScriptLogo } from './icons';

const LANGUAGES: {
  name: string;
  logo: Component<{ size?: number }>;
  class: string;
  text: string;
}[] = [
  {
    name: 'TypeScript',
    logo: TypeScriptLogo,
    class: 'lang-ts',
    text: 'IntelliSense across the whole project, definitions a Ctrl+click away, and tsc after every change the agent makes.',
  },
  {
    name: 'JavaScript',
    logo: JavaScriptLogo,
    class: 'lang-js',
    text: 'The same project-wide IntelliSense, with your lint and tests run after every change.',
  },
  {
    name: 'Rust',
    logo: RustLogo,
    class: 'lang-rs',
    text: 'rust-analyzer in the editor, and cargo check, clippy and tests after every change.',
  },
];

export default function Status() {
  return (
    <section class="section" id="status">
      <div class="container status">
        <div class="status-copy">
          <p class="eyebrow">
            <span class="live-dot" aria-hidden="true" />
            Status
          </p>
          <h2>In active development</h2>
          <p>
            Rovik is young and moving fast. It supports JavaScript, TypeScript and Rust today, and
            more languages will follow. Expect rough edges and frequent changes; bug reports and
            ideas are welcome.
          </p>
          <a class="text-link" href={ISSUES_URL}>
            Open an issue on GitHub
            <ArrowRightIcon size={16} />
          </a>
        </div>
        <ul class="langs">
          <For each={LANGUAGES}>
            {(lang) => {
              const Logo = lang.logo;
              return (
                <li class="lang">
                  <span class={`lang-logo ${lang.class}`}>
                    <Logo size={22} />
                  </span>
                  <div>
                    <h3>{lang.name}</h3>
                    <p>{lang.text}</p>
                  </div>
                </li>
              );
            }}
          </For>
        </ul>
      </div>
    </section>
  );
}
