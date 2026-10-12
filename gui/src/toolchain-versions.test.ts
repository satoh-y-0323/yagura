/**
 * Floor assertions on the toolchain versions `gui/package-lock.json` resolved. Every
 * assertion is a ">= major" floor, so only a major version going backwards is
 * detected; narrower lines are guarded by the ranges in `package.json`.
 *
 * The lockfile is a build-time static asset rather than runtime external input, so it
 * is read through a type-annotated assignment plus runtime key guards instead of the
 * `unknown`-then-narrow rule for external boundaries.
 */
import { describe, it, expect } from 'vitest';
import lock from '../package-lock.json';

const typed: { packages: Record<string, { version?: string } | undefined> } = lock;

interface VersionFloor {
  readonly packageName: string;
  readonly lockKey: string;
  readonly minimumMajor: number;
}

const VERSION_FLOORS: readonly VersionFloor[] = [
  { packageName: 'vite', lockKey: 'node_modules/vite', minimumMajor: 8 },
  { packageName: 'vitest', lockKey: 'node_modules/vitest', minimumMajor: 4 },
  {
    packageName: '@sveltejs/vite-plugin-svelte',
    lockKey: 'node_modules/@sveltejs/vite-plugin-svelte',
    minimumMajor: 7,
  },
  { packageName: 'eslint', lockKey: 'node_modules/eslint', minimumMajor: 9 },
  {
    packageName: 'eslint-plugin-svelte',
    lockKey: 'node_modules/eslint-plugin-svelte',
    minimumMajor: 3,
  },
];

function resolvedVersion(lockKey: string): string {
  const entry = typed.packages[lockKey];
  if (entry === undefined) {
    throw new Error(`package-lock.json has no "packages" entry for ${lockKey}`);
  }
  const version = entry.version;
  if (version === undefined) {
    throw new Error(`package-lock.json entry ${lockKey} carries no version field`);
  }
  return version;
}

function majorOf(version: string): number {
  const major = parseInt(version, 10);
  if (Number.isNaN(major)) {
    throw new Error(`version string ${version} does not start with a major number`);
  }
  return major;
}

describe('package-lock.json toolchain versions', () => {
  for (const floor of VERSION_FLOORS) {
    it(`resolves ${floor.packageName} at major ${floor.minimumMajor} or newer`, () => {
      expect(majorOf(resolvedVersion(floor.lockKey))).toBeGreaterThanOrEqual(floor.minimumMajor);
    });
  }
});
