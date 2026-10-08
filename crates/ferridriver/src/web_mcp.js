const model = document.modelContext ?? navigator.modelContext;
if (!model) return [false];
const prototype = Object.getPrototypeOf(model);
const invoke = prototype?.invokeTool ?? model.invokeTool;
if (toolName !== null && typeof invoke === 'function') return [true, await invoke.call(model, toolName, input ?? {})];
const discover = prototype?.getTools ?? model.getTools;
if (typeof discover !== 'function') return [false];
const tools = (await discover.call(model)).filter(tool => !('window' in tool) || tool.window === window);
const formFor = name => {
  for (const form of document.querySelectorAll('form[toolname]')) {
    if (form.getAttribute('toolname') === name) return form;
  }
  return null;
};
if (toolName === null) {
  return [true, tools.map(tool => {
    const info = {name: tool.name, description: tool.description ?? ''};
    if (typeof tool.title === 'string' && tool.title !== '') info.title = tool.title;
    if (tool.inputSchema !== undefined) {
      info.inputSchema = typeof tool.inputSchema === 'string' ? JSON.parse(tool.inputSchema) : tool.inputSchema;
    }
    const annotations = {};
    const hints = tool.annotations;
    if (hints) {
      for (const name of ['readOnly', 'untrustedContent', 'consequential']) {
        if (hints[name + 'Hint'] === true) annotations[name] = true;
      }
      if (hints.debugging === true) annotations.debugging = true;
    }
    const form = formFor(tool.name);
    if (form?.hasAttribute('toolautosubmit')) annotations.autosubmit = true;
    if (Object.keys(annotations).length) info.annotations = annotations;
    if (typeof tool.origin === 'string' && tool.origin !== '') info.origin = tool.origin;
    if (form) info.declarative = true;
    return info;
  })];
}
const tool = tools.find(tool => tool.name === toolName);
if (!tool) {
  const available = tools.map(tool => tool.name);
  throw new Error(`No WebMCP tool named "${toolName}".` +
    (available.length ? ` Available tools: ${available.join(', ')}.` : ' The frame does not register any WebMCP tools.'));
}
const execute = prototype?.executeTool ?? model.executeTool;
if (typeof execute !== 'function') return [false];
// Chromium before 155 requires JSON text. Choose before invoking, never replay a tool on failure.
const encoded = typeof tool.inputSchema === 'string' || (tool.inputSchema === undefined && legacyInput);
// The caller stops waiting at its deadline; abort the call there too, so the
// tool is told through its signal instead of running on unobserved.
const controller = new AbortController();
const timer = deadline > 0 ? setTimeout(() => controller.abort(new DOMException('deadline', 'TimeoutError')), deadline) : null;
let result;
try {
  result = await execute.call(model, tool, encoded ? JSON.stringify(input ?? {}) : (input ?? {}), {signal: controller.signal});
} catch (error) {
  if (controller.signal.aborted) return ['timeout'];
  throw error;
} finally {
  if (timer !== null) clearTimeout(timer);
}
if (typeof result === 'string') {
  if (result === 'undefined') result = undefined;
  else {
    try { result = JSON.parse(result); } catch {}
  }
}
return [true, result];
