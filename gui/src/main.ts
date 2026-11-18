import { mount } from 'svelte';

import App from './App.svelte';
import { sharedInputModality } from './components/input-modality';
import { suppressNativeContextMenu } from './native-context-menu';
import { sealObjectPrototype } from './prototype-seal';
import './styles/tokens.css';
import './styles/components.css';
import { applyPalette } from './theme/theme-apply';
import { createBrowserThemeDeps, startupPalette } from './theme/theme-store';

// Runs before the app mounts, so no value crossing the IPC edge is ever read
// under a prototype that can still be given a wire field name.
sealObjectPrototype();

// Bubbles last, so a window that handled its own right-click is seen as such.
document.addEventListener('contextmenu', suppressNativeContextMenu);

// The stylesheets define no palette colors, so the first paint needs them
// before the app mounts. This writes the palette the app's own store starts on
// (the stored selection or the default) once; a named user theme and later
// changes are applied by the app's store.
applyPalette(document.documentElement, startupPalette(createBrowserThemeDeps()));

// Before the app mounts: listeners on one target run in the order they were added,
// so the tracker sees a key before a shortcut that moves focus on it.
sharedInputModality();

const target = document.getElementById('app');
if (target === null) {
  throw new Error('missing #app mount point');
}

export default mount(App, { target });
