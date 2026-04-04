/**
 * @ferridriver/ct-vue — registerSource
 *
 * Implements window.__ferriMount/Update/Unmount using Vue 3's createApp.
 */

import { createApp, h } from "vue";

const __apps = new Map();

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

  const app = createApp({
    render() {
      return h(Component, props);
    },
  });

  // Allow plugins via beforeMount hooks (e.g. Pinia, Vue Router).
  if (options.hooksConfig && options.hooksConfig.plugins) {
    for (const plugin of options.hooksConfig.plugins) {
      app.use(plugin);
    }
  }

  app.mount(rootElement);
  __apps.set(rootElement, { app, Component });

  if (window.__ferriAfterMount) {
    for (const hook of window.__ferriAfterMount) {
      await hook({ Component, props, rootElement, hooksConfig: options.hooksConfig });
    }
  }
};

window.__ferriUpdate = function (rootElement, newProps) {
  const entry = __apps.get(rootElement);
  if (!entry) throw new Error("No component mounted on this element");
  entry.app.unmount();
  const app = createApp({ render: () => h(entry.Component, newProps) });
  app.mount(rootElement);
  entry.app = app;
};

window.__ferriUnmount = function (rootElement) {
  const el = rootElement || document.getElementById("root") || document.getElementById("app");
  const entry = __apps.get(el);
  if (entry) {
    entry.app.unmount();
    __apps.delete(el);
  }
};

window.__ferriBeforeMount = window.__ferriBeforeMount || [];
window.__ferriAfterMount = window.__ferriAfterMount || [];
