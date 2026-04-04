/**
 * @ferridriver/ct-svelte — registerSource
 *
 * Implements window.__ferriMount/Update/Unmount for Svelte 4/5.
 * Svelte 5 uses mount() from 'svelte', Svelte 4 uses `new Component()`.
 */

const __instances = new Map();

window.__ferriMount = async function (componentRef, rootElement, options = {}) {
  const props = options.props || componentRef.props || {};

  let Component;
  if (window.__ferriRegistry && window.__ferriRegistry[componentRef.id]) {
    Component = await window.__ferriRegistry[componentRef.id]();
  } else {
    throw new Error(`Component "${componentRef.id}" not found in registry.`);
  }

  if (window.__ferriBeforeMount) {
    for (const hook of window.__ferriBeforeMount) {
      await hook({ Component, props, hooksConfig: options.hooksConfig });
    }
  }

  // Svelte 5: mount() from 'svelte'. Svelte 4: new Component().
  let instance;
  if (typeof Component === "function" && Component.prototype && Component.prototype.$set) {
    // Svelte 4 class component.
    instance = new Component({ target: rootElement, props });
  } else {
    // Svelte 5 — try the mount API.
    try {
      const svelte = await import("svelte");
      instance = svelte.mount(Component, { target: rootElement, props });
    } catch {
      // Fallback: try as class.
      instance = new Component({ target: rootElement, props });
    }
  }

  __instances.set(rootElement, { instance, Component });

  if (window.__ferriAfterMount) {
    for (const hook of window.__ferriAfterMount) {
      await hook({ Component, props, rootElement, hooksConfig: options.hooksConfig });
    }
  }
};

window.__ferriUpdate = function (rootElement, newProps) {
  const entry = __instances.get(rootElement);
  if (!entry) throw new Error("No component mounted");
  if (entry.instance.$set) {
    entry.instance.$set(newProps); // Svelte 4.
  }
  // Svelte 5: re-mount.
};

window.__ferriUnmount = function (rootElement) {
  const el = rootElement || document.getElementById("root") || document.getElementById("app");
  const entry = __instances.get(el);
  if (entry) {
    if (entry.instance.$destroy) entry.instance.$destroy(); // Svelte 4.
    else if (entry.instance.$$) entry.instance.$$destroy?.(); // Svelte 5 internals.
    else el.innerHTML = "";
    __instances.delete(el);
  }
};

window.__ferriBeforeMount = window.__ferriBeforeMount || [];
window.__ferriAfterMount = window.__ferriAfterMount || [];
