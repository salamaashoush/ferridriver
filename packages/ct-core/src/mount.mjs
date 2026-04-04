/**
 * ferridriver CT mount logic.
 *
 * Provides the `mount()` function that:
 * 1. Serializes the JSX component tree (replacing functions with ordinal refs)
 * 2. Calls page.evaluate() to send it to the browser
 * 3. Browser-side unwrapObject() resolves importRefs + function refs
 * 4. Framework's __ferriMount() renders the real component
 * 5. Returns a Locator pointing at the mounted component root
 */

// ── Serialization: wrap functions as ordinal refs before sending to browser ──

/**
 * Walk the component tree, replacing JS functions with { __pw_type: 'function', ordinal: N }.
 * The ordinal indexes into the callbacks array, which is bridged to the browser
 * via page.exposeFunction('__ferriDispatchFunction', ...).
 */
export function wrapObject(value, callbacks) {
  return transformObject(value, (v) => {
    if (typeof v === "function") {
      const ordinal = callbacks.length;
      callbacks.push(v);
      return { result: { __pw_type: "function", ordinal } };
    }
  });
}

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
 * Create a component descriptor from user input.
 * If it's already a JSX descriptor ({ __pw_type: 'jsx' }), pass through.
 * Otherwise wrap it as an object-component.
 */
export function createComponent(component, options = {}) {
  if (component && component.__pw_type === "jsx") {
    return component;
  }
  // Object notation: mount(Counter, { props: { initial: 5 } })
  return {
    __pw_type: "jsx",
    type: component, // should be an importRef
    props: options.props || {},
    key: undefined,
  };
}

/**
 * Mount a component in the browser page.
 *
 * @param {Page} page - ferridriver Page instance
 * @param {any} componentRef - JSX descriptor or importRef
 * @param {object} options - { props?, hooksConfig?, slots? }
 * @param {Function[]} boundCallbacks - mutable array for callback bridge
 * @returns {Promise<Locator>} - Locator pointing at #root
 */
export async function mount(page, componentRef, options = {}, boundCallbacks) {
  // Serialize: replace JS functions with ordinal refs.
  const component = wrapObject(
    createComponent(componentRef, options),
    boundCallbacks
  );
  const hooksConfig = wrapObject(options.hooksConfig || {}, boundCallbacks);

  // Wait for the framework registerSource to be ready.
  await page.evaluate(
    `(() => new Promise(r => { const c = () => window.__ferriMount ? r() : setTimeout(c, 10); c(); }))()`
  );

  // Send the serialized component to the browser.
  // The browser-side __ferriUnwrapObject resolves importRefs (via dynamic import)
  // and function refs (via __ferriDispatchFunction callback bridge).
  const componentJson = JSON.stringify(component);
  const hooksJson = JSON.stringify(hooksConfig);

  await page.evaluate(`(async () => {
    const component = await window.__ferriUnwrapObject(${componentJson});
    const hooksConfig = await window.__ferriUnwrapObject(${hooksJson});
    let rootElement = document.getElementById('root');
    if (!rootElement) {
      rootElement = document.createElement('div');
      rootElement.id = 'root';
      document.body.appendChild(rootElement);
    }
    await window.__ferriMount(component, rootElement, hooksConfig);
  })()`);

  return page.locator("#root");
}

/**
 * Unmount the component at #root.
 */
export async function unmount(page) {
  await page.evaluate(`(() => {
    const root = document.getElementById('root');
    if (root && window.__ferriUnmount) window.__ferriUnmount(root);
  })()`);
}

/**
 * Update props on the mounted component.
 */
export async function update(page, options = {}, boundCallbacks) {
  const wrapped = wrapObject(options, boundCallbacks);
  const json = JSON.stringify(wrapped);
  await page.evaluate(`(async () => {
    const options = await window.__ferriUnwrapObject(${json});
    const root = document.getElementById('root');
    if (root && window.__ferriUpdate) window.__ferriUpdate(root, options);
  })()`);
}
