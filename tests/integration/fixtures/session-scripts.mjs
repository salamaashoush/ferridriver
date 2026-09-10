export const scripts = {
  "globals_persist_across_executions": [
    "globalThis.h = () => 42; return null;",
    "return h();"
  ],
  "let_const_inside_call_do_not_persist": [
    "let x = 5; return x;",
    "return typeof x;"
  ],
  "plain_throw_does_not_poison_and_state_survives": [
    "globalThis.keep = 'alive'; return 1;",
    "throw new Error('boom');",
    "return globalThis.keep;"
  ],
  "timeout_poisons_the_session": [
    "while (true) { /* spin */ }"
  ],
  "native_await_park_hits_the_backstop_and_poisons": [
    "await new Promise(() => {});"
  ],
  "finished_call_deadline_does_not_halt_later_vm_entry": [
    "return 1;"
  ],
  "framework_globals_refresh_each_call": [
    "vars.set('k', 'v'); return null;",
    "return vars.get('k');"
  ],
  "set_timeout_resolves_inside_execute": [
    "return await new Promise((resolve) => setTimeout(() => resolve(7), 20));"
  ],
  "timer_handle_persists_and_clears_across_calls": [
    "globalThis.__t = setTimeout(() => { globalThis.__fired = true; }, 10000); return typeof globalThis.__t;",
    "clearTimeout(globalThis.__t); return globalThis.__fired === true;"
  ],
  "url_and_search_params_work": [
    "const p = new URLSearchParams('a=1&b=2'); p.append('b', '3'); return [p.get('a'), p.getAll('b').join(',')];"
  ],
  "web_polyfills_text_codec_base64_microtask": [
    "const enc = new TextEncoder().encode('hi€'); const dec = new TextDecoder().decode(enc); let mt = 0; queueMicrotask(() => { mt = 1; }); await Promise.resolve(); return { len: enc.length, dec, b64: btoa('hi'), round: atob(btoa('xy')), mt };"
  ],
  "console_uses_node_style_formatter_and_is_captured": [
    "console.log('n =', 42, { a: 1 }); console.warn(['x', 'y']); return null;"
  ],
  "native_url_class_parses_and_exposes_search_params": [
    "const u = new URL('https://ex.com:8443/a/b?x=1&y=2#frag'); return { href: u.href, host: u.host, hostname: u.hostname, port: u.port, proto: u.protocol, path: u.pathname, search: u.search, hash: u.hash, origin: u.origin, sp: u.searchParams.get('y'), str: String(u) };"
  ],
  "assertion_failures_throw_a_named_assertion_error": [
    "try { expect(1).toBe(2); return 'no-throw'; } catch (e) { return { name: e.name, isError: e instanceof Error }; }"
  ],
  "set_timeout_passes_extra_args_and_clear_tolerates_garbage": [
    "clearTimeout(undefined); clearTimeout(null); clearTimeout(42); clearInterval();\nreturn await new Promise((resolve) => setTimeout((a, b) => resolve(a + b), 10, 'x', 'y'));"
  ],
  "url_search_params_binding_is_live_in_both_directions": [
    "const u = new URL('https://ex.com/p?a=1');\nconst sp = u.searchParams;\nsp.append('b', '2');\nconst afterAppend = [u.href, u.search];\nu.search = '?c=3';\nconst afterSearchSet = [sp.get('c'), sp.has('a'), sp.size];\nu.href = 'https://ex.com/q?d=4';\nreturn { afterAppend, afterSearchSet, sameObject: u.searchParams === sp, afterHref: sp.get('d') };"
  ],
  "text_codecs_cover_utf16_and_the_stream_forms": [
    "const utf16 = new TextDecoder('utf-16le').decode(new Uint8Array([0x68, 0x00, 0x69, 0x00]));\nconst es = new TextEncoderStream();\nconst ds = new TextDecoderStream();\nconst out = es.readable.pipeThrough(ds).getReader();\nconst w = es.writable.getWriter();\nawait w.write('hi\\u20ac');\nawait w.close();\nlet text = '';\nfor (;;) { const { value, done } = await out.read(); if (done) break; text += value; }\nreturn { utf16, text, encoding: ds.encoding };"
  ],
  "node_url_module_serves_the_path_and_host_helpers": [
    "const url = require('node:url');\nconst opts = url.urlToHttpOptions(new URL('https://ex.com:8443/a?b=1'));\nreturn {\npath: url.fileURLToPath('file:///tmp/a b.txt'),\nhref: url.pathToFileURL('/tmp/a b.txt').href,\nascii: url.domainToASCII('bücher.de'),\nunicode: url.domainToUnicode('xn--bcher-kva.de'),\nsameClass: url.URL === URL,\nport: opts.port,\nsearch: opts.search,\n};"
  ],
  "url_search_params_node_semantics": [
    "const fromNull = new URLSearchParams(null).toString();\nconst enc = new URLSearchParams('a=1 2&b=%C3%A9');\nconst encoded = enc.toString();\nconst decoded = enc.get('b');\nconst live = new URLSearchParams('a=1&b=2&c=3');\nfor (const [k] of live) { live.delete(k); }\nconst empty = new URLSearchParams('').size;\nconst s = new URLSearchParams('b=2&a=1&a=0'); s.sort();\nreturn [fromNull, encoded, decoded, live.size, empty, s.toString()];"
  ],
  "console_printf_and_inspect_rendering": [
    "console.log('%s scored %d%%', 'sashoush', 97, 'extra');\nconsole.log(['x', 1]);\nconsole.log(new Map([['a', 1]]));\nconsole.log(new Set([1, 2]));\nconsole.log(/ab+c/gi);\nreturn null;"
  ]
};
