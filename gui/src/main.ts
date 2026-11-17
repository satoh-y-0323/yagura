import { mount } from 'svelte';

import App from './App.svelte';
import { suppressNativeContextMenu } from './native-context-menu';
import { sealObjectPrototype } from './prototype-seal';
import './styles/tokens.css';
import './styles/components.css';
import { bindPalette } from './theme/theme-apply';
import { createBrowserThemeDeps, createThemeStore } from './theme/theme-store';

// Runs before the app mounts, so no value crossing the IPC edge is ever read
// under a prototype that can still be given a wire field name.
sealObjectPrototype();

// Bubbles last, so a window that handled its own right-click is seen as such.
document.addEventListener('contextmenu', suppressNativeContextMenu);

// The stylesheets define no palette colors, so the first paint needs them
// before the app mounts. This store only picks the palette the app's own store
// starts on (the stored selection or the default); a named user theme is
// applied by the app's store once the themes file is read.
bindPalette(
  createThemeStore(createBrowserThemeDeps()),
  (state) => state.appPalette,
  document.documentElement
)();

const target = document.getElementById('app');
if (target === null) {
  throw new Error('missing #app mount point');
}

export default mount(App, { target });
