import { hydrate, render } from 'solid-js/web';
import '@fontsource-variable/inter';
import '@fontsource-variable/jetbrains-mono';
import './styles.css';
import App from './App';

const root = document.getElementById('root')!;
// The build prerenders the page (scripts/prerender.mjs); the dev server
// serves it empty.
if (root.firstElementChild) {
  hydrate(() => <App />, root);
} else {
  render(() => <App />, root);
}
