/**
 * ferridriver CT browser runtime.
 *
 * Injected into the Vite bundle. Sets up:
 * - ImportRegistry: maps component IDs to lazy import() functions
 * - unwrapObject: deserializes importRefs and function refs
 * - wrapObject helper for the Node side (not used here, but exported for symmetry)
 *
 * Framework registerSource (e.g. ct-react/registerSource.mjs) defines
 * window.__ferriMount/Update/Unmount using this registry.
 */

// ── Import Registry ──

class ImportRegistry {
  constructor() {
    this._registry = new Map();
  }

  initialize(components) {
    for (const [name, importFn] of Object.entries(components)) {
      this._registry.set(name, importFn);
    }
  }

  async resolveImportRef(ref) {
    const importFn = this._registry.get(ref.id);
    if (!importFn) {
      throw new Error(
        `Component "${ref.id}" not registered. Available: [${[...this._registry.keys()].join(", ")}]`
      );
    }
    let module = await importFn();
    if (ref.property) {
      module = module[ref.property];
    }
    return module;
  }
}

// ── Serialization: unwrap importRefs and function refs ──

function isImportRef(v) {
  return v && typeof v === "object" && v.__pw_type === "importRef";
}

function isFunctionRef(v) {
  return v && typeof v === "object" && v.__pw_type === "function";
}

function isJsxComponent(v) {
  return v && typeof v === "object" && v.__pw_type === "jsx";
}

/**
 * Recursively walk an object, transforming special types.
 * The visitor returns { result } to replace a value, or undefined to keep it.
 */
async function transformObjectAsync(value, visitor) {
  const transformed = await visitor(value);
  if (transformed !== undefined) return transformed.result;

  if (value === null || value === undefined) return value;
  if (typeof value !== "object") return value;

  if (Array.isArray(value)) {
    const result = [];
    for (const item of value) {
      result.push(await transformObjectAsync(item, visitor));
    }
    return result;
  }

  const result = {};
  for (const [key, val] of Object.entries(value)) {
    result[key] = await transformObjectAsync(val, visitor);
  }
  return result;
}

/**
 * Synchronous transform (no async resolution needed).
 */
function transformObject(value, visitor) {
  const transformed = visitor(value);
  if (transformed !== undefined) return transformed.result;

  if (value === null || value === undefined) return value;
  if (typeof value !== "object") return value;

  if (Array.isArray(value)) {
    return value.map((item) => transformObject(item, visitor));
  }

  const result = {};
  for (const [key, val] of Object.entries(value)) {
    result[key] = transformObject(val, visitor);
  }
  return result;
}

/**
 * Browser-side: resolve importRefs to real modules, function refs to callback wrappers.
 */
async function unwrapObject(value) {
  return transformObjectAsync(value, async (v) => {
    if (isFunctionRef(v)) {
      // Create a wrapper that calls back to Node.js via exposeFunction.
      const result = (...args) => {
        if (window.__ferriDispatchFunction) {
          window.__ferriDispatchFunction(v.ordinal, args);
        }
      };
      return { result };
    }
    if (isImportRef(v)) {
      return { result: await window.__ferriRegistry.resolveImportRef(v) };
    }
  });
}

// ── Install globals ──

window.__ferriRegistry = new ImportRegistry();
window.__ferriUnwrapObject = unwrapObject;
window.__ferriTransformObject = transformObject;
window.__ferriIsJsxComponent = isJsxComponent;
