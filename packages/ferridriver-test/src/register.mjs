/**
 * Preload: registers the .js→.ts resolve hook before any imports.
 * Used via: node --import ./register.mjs
 */
import { register } from 'node:module';
register(new URL('./loader.mjs', import.meta.url));
