import { render } from 'solid-js/web';
import App from './App';
import './styles.css';
import './lib/monacoSetup';
import { installMiddleClickPasteGuard } from './lib/middleClickPaste';
import { installShortcutKeys } from './lib/shortcutKeys';

installMiddleClickPasteGuard();
installShortcutKeys();

const root = document.getElementById('root');
render(() => <App />, root!);
