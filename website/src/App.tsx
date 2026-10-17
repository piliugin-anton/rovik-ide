import { createSignal, onMount } from 'solid-js';
import Downloads from './components/Downloads';
import Features from './components/Features';
import Footer from './components/Footer';
import Header from './components/Header';
import Hero from './components/Hero';
import Showcase from './components/Showcase';
import Status from './components/Status';
import { detectVisitor, type Visitor } from './lib/platform';
import { loadLatestRelease, type ReleaseState } from './lib/release';

export default function App() {
  // Unknown in the prerendered page, as in the first render that hydrates
  // it; the browser fills them in afterwards.
  const [visitor, setVisitor] = createSignal<Visitor | null>(null);
  const [release, setRelease] = createSignal<ReleaseState>({ status: 'loading' });

  onMount(() => {
    void detectVisitor().then(setVisitor);
    void loadLatestRelease().then(setRelease);
  });

  return (
    <>
      <Header />
      <main>
        <Hero visitor={visitor()} release={release()} />
        <Showcase />
        <Features />
        <Status />
        <Downloads visitor={visitor()} release={release()} />
      </main>
      <Footer />
    </>
  );
}
