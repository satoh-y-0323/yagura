import { mount } from 'svelte';

import App from './App.svelte';
import { suppressNativeContextMenu } from './native-context-menu';
import { sealObjectPrototype } from './prototype-seal';
import './styles/tokens.css';
import './styles/components.css';

// Runs before the app mounts, so no value crossing the IPC edge is ever read
// under a prototype that can still be given a wire field name.
sealObjectPrototype();

// Bubbles last, so a window that handled its own right-click is seen as such.
document.addEventListener('contextmenu', suppressNativeContextMenu);

const target = document.getElementById('app');
if (target === null) {
  throw new Error('missing #app mount point');
}

export default mount(App, { target });
