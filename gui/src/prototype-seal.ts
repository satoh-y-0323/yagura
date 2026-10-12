/**
 * Seals `Object.prototype` so nothing running in the page can add a name to
 * it. A wire field can only be impersonated by a name the prototype does not
 * already carry, so forbidding additions closes the supply; freezing would
 * close it too, but it also makes every inherited name non-writable, and an
 * object that shadows one by assignment -- the shape a compiled TypeScript
 * namespace takes, which the bundled terminal emulator ships -- then throws
 * under the strict mode every module runs in.
 *
 * Sealing does not make the members `Object.prototype` already has read-only,
 * and does not stop a value from being given a prototype of its own, so
 * reading wire values through an ownership check stays necessary.
 */
export function sealObjectPrototype(): void {
  Object.seal(Object.prototype);
}
