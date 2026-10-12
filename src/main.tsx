import { render } from 'solid-js/web';
import App from './App';
import './styles.css';
import './lib/monacoSetup';
import { installMiddleClickPasteGuard } from './lib/middleClickPaste';
import { dismissBootSplash } from './lib/bootSplash';

installMiddleClickPasteGuard();

const root = document.getElementById('root');
render(() => <App />, root!);
// Never leave the boot splash stuck if project restore hangs.
setTimeout(dismissBootSplash, 8000);
