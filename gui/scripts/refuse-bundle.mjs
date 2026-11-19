#!/usr/bin/env node
// The base config's `beforeBundleCommand`. That config names no sidecars or resources, so a bundle
// from it alone would ship an app missing them; only each OS's distribution config replaces this.
console.error(
  'refuse-bundle: this configuration does not bundle; build a distribution with ' +
    '`npm run build:tauri`, which bundles with the distribution config for this OS.'
);
process.exit(1);
