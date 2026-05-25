# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Bug Fixes

- *(script)* Bind allow.net to global fetch, closing a net-guard bypass ([3a7ac0a](https://github.com/salamaashoush/ferridriver/commit/3a7ac0a6cf6fbd8dcf2100eec0423d89bd14b66d))
- *(http_client)* Honour per-request max_redirects (was a no-op) ([eecfcf5](https://github.com/salamaashoush/ferridriver/commit/eecfcf570458f9bde94c714222b57112fe612350))
- *(webkit)* Middle-click no longer fails the hit-target interceptor ([939785e](https://github.com/salamaashoush/ferridriver/commit/939785efb354fc09745926b768af4a4c52939c27))
- *(bdd,cdp)* Parallelism, close-path stalls, cookie ctx, JS-driven gaps ([cfe0c26](https://github.com/salamaashoush/ferridriver/commit/cfe0c26ff4e83d91d829b44c4eaad1ca393636b0))
- *(tests)* World-first hook/step args + skip webkit utility-script tests ([203b362](https://github.com/salamaashoush/ferridriver/commit/203b3626885441ea9c7e02a727481315348a233e))
- *(pw_webkit)* Lazy file-chooser intercept to unwedge matrix ([d2bcf0e](https://github.com/salamaashoush/ferridriver/commit/d2bcf0efecc8a856b5a0e9a8e8eed30950cf0387))
- *(pw_webkit)* Handle provisional target swap on cross-process navigation ([737f14a](https://github.com/salamaashoush/ferridriver/commit/737f14a9b05460c540c12cb6aa7fc298f1eda718))
- *(pw_webkit)* Defer Download event until suggestedFilename arrives ([9c36cdd](https://github.com/salamaashoush/ferridriver/commit/9c36cddfcc3689e7d1550456b80e1a1d2f23a6c2))

### Features

- *(script)* Session lifetime model, hardened commands, process/fetch, extension fixes ([6e60ac1](https://github.com/salamaashoush/ferridriver/commit/6e60ac11e7a0a0913a0a3555119b94bd5f37d284))
- *(fetch)* WHATWG-spec Headers (subset, no deps) ([b856aa8](https://github.com/salamaashoush/ferridriver/commit/b856aa8fb03f0e4b198f8f7cbd402eee0fa3fc5f))
- *(fetch)* Standard Request/Response globals; de-globalise network classes ([e58f32d](https://github.com/salamaashoush/ferridriver/commit/e58f32d7cb06525295857d110e17ee28f659d210))
- *(abort)* AbortController/AbortSignal + fetch signal ([9d78fd9](https://github.com/salamaashoush/ferridriver/commit/9d78fd9f9bf0ede04d188a74ea6fc1f9111f06f0))
- *(streams)* ReadableStream subset; Response.body is a stream ([5dac77a](https://github.com/salamaashoush/ferridriver/commit/5dac77a0d9765a2434854f417fcd7e20a5f3b8c6))
- *(fetch)* Blob + FormData (spec subset) as fetch bodies ([cffadd1](https://github.com/salamaashoush/ferridriver/commit/cffadd1cef6aa5308906c8d7f3d542f2fe653b0d))
- *(fetch)* Incremental Response.body streaming (no full buffering) ([2c960df](https://github.com/salamaashoush/ferridriver/commit/2c960df6bf0d89f5ac031a693633c06ed04d69e0))
- *(process)* Stdout/stderr.write, hrtime.bigint, document nextTick order ([de87a35](https://github.com/salamaashoush/ferridriver/commit/de87a35a7ab40540990254f93c4e61b09978ad75))
- *(bdd)* Thread [scripting] env caps into the step VM; remove nodeCompat ([49f2ae9](https://github.com/salamaashoush/ferridriver/commit/49f2ae99dcaad44e68c1b88a6dc3cdd570a664d8))
- *(expect)* Add ferridriver-expect crate ([7568a50](https://github.com/salamaashoush/ferridriver/commit/7568a503740c864938e22b96314ef62cbabafedb))
- *(script)* QuickJS expect() global ([d374949](https://github.com/salamaashoush/ferridriver/commit/d3749498c7b453e69bba86b3e1bdec980bf71742))
- *(bdd)* Uniform world-as-first-arg for steps and hooks ([c03eda2](https://github.com/salamaashoush/ferridriver/commit/c03eda201c5f38159ff2b972fc671283c130b2de))
- *(reporter)* Colorize unified-diff lines in terminal reporter ([7c8d873](https://github.com/salamaashoush/ferridriver/commit/7c8d873481c81680dd8ab4ce48f895e850d268e8))
- *(http)* NetGuard sandbox network policy + fetch binding hardening ([5e20eab](https://github.com/salamaashoush/ferridriver/commit/5e20eab2a088f3b56d873023cac63fa6158a6232))
- *(page)* Async exposeFunction, cookie url field, CDP modifier combos ([a14e9f1](https://github.com/salamaashoush/ferridriver/commit/a14e9f11c9639b3bf9bd2d16bfb16da5dd9fe21e))
- *(script,node)* Expanded JS / NAPI binding surface ([cefb6cb](https://github.com/salamaashoush/ferridriver/commit/cefb6cb71ea4161ad7a6b661fd31afca991bc608))
- *(webkit)* Linux port via gtk4/webkit6 host + cross-platform host crate ([89104f4](https://github.com/salamaashoush/ferridriver/commit/89104f42628fb541171858a6bf421568071f6941))
- *(backend)* Scaffold Playwright WebKit backend (pw_webkit) ([d2d5fab](https://github.com/salamaashoush/ferridriver/commit/d2d5fabe6d3af1f41c0527b8a310a492674f0db4))
- *(pw_webkit)* Full backend wiring — Browser/Page/Element + AnyPage dispatch ([9bc3f77](https://github.com/salamaashoush/ferridriver/commit/9bc3f771f920ef606609e5fbe68eeb04e3d2bff5))
- *(pw_webkit)* Route interception, frame contexts, more matrix fixes ([8104d46](https://github.com/salamaashoush/ferridriver/commit/8104d46e59070b1e20bcb0b60b35c658df35999f))
- *(pw_webkit)* Wire WS events + context-options pre-page hook + page_backref fix ([ff85663](https://github.com/salamaashoush/ferridriver/commit/ff85663309a54e5b39364150aabce4ed9b1f11d9))

### Miscellaneous

- *(ci)* Unblock CI -- fmt + clippy + typos + rustdoc ([c51e58f](https://github.com/salamaashoush/ferridriver/commit/c51e58f2c2a028584667bfd3989f8a67f45fc653))

### Performance

- Worker-scoped request, pre-warm, async tempdir, bidi close ([c74260f](https://github.com/salamaashoush/ferridriver/commit/c74260f2f43f2079f98b58c4e62bc974643c1f23))

### Refactoring

- Rename api_request -> http_client, drop API-testing framing ([26e380f](https://github.com/salamaashoush/ferridriver/commit/26e380f2e61576a7c94bd3a5067b787abc5d2f38))
- *(test)* Consolidate expect on ferridriver-expect ([854eea8](https://github.com/salamaashoush/ferridriver/commit/854eea869b0f001b06cc37503f76344255374a13))
- *(bdd-steps)* Migrate built-in step helpers to AssertionFailure ([2511f97](https://github.com/salamaashoush/ferridriver/commit/2511f976ced10d981fcbf733b8ae68598e8d503c))
- Consolidate webkit backend (drop legacy, promote pw-webkit) ([337671b](https://github.com/salamaashoush/ferridriver/commit/337671b1fdc17e1e5608ba26f750617951a98cde))

### Testing

- *(cli,bdd-example)* Cross-backend expect coverage + Rust value-matcher demo ([f239696](https://github.com/salamaashoush/ferridriver/commit/f23969653e12b2e92ed12fcc452e8c1fc924363e))
- *(pw_webkit)* Drop run_cdp! gates on emulation tests, achieve full parity ([53ca4b5](https://github.com/salamaashoush/ferridriver/commit/53ca4b5bb3252f6c2a01ec0d4fba6c6d8448ed39))

## [0.2.0] - 2026-05-18

### Bug Fixes

- *(test-runner)* Lazy browser launch via BrowserHandle ([e0b8168](https://github.com/salamaashoush/ferridriver/commit/e0b81687a279f715faf96e1a8897f0b71457898c))
- SetInputFiles via objectId, BiDi/WebKit frame name sync, Firefox in CI ([981fe3c](https://github.com/salamaashoush/ferridriver/commit/981fe3caca0cf66d1d955d2b73b3ae30b700a274))
- *(bidi)* Name fallback to window.name; goto retries sync_frames ([e08fc1b](https://github.com/salamaashoush/ferridriver/commit/e08fc1be3fabf1095cc470ca0f91752d65f37d26))
- *(bidi)* Single sync_frames pass to keep dialog timing intact ([9b4e1a1](https://github.com/salamaashoush/ferridriver/commit/9b4e1a195f7826c6d5c2153a2c37ec12f0f83ae3))
- *(bidi)* Retry frame name resolution up to 3 rounds ([121cd01](https://github.com/salamaashoush/ferridriver/commit/121cd013a29588c44b06ea1d9cc1382d0f5e5b14))
- *(bidi)* Pass serializationOptions to locateNodes for attributes ([825d5c8](https://github.com/salamaashoush/ferridriver/commit/825d5c8d32fab4240f23733ed4a3aa584eae9193))
- *(cdp)* Disable private-network-request blocking flags ([c48cd9d](https://github.com/salamaashoush/ferridriver/commit/c48cd9dc4682b3a5cef43fcc086ee32f8cec36b8))
- *(state)* Skip snap-wrapped Firefox in detect_firefox ([0b42e7c](https://github.com/salamaashoush/ferridriver/commit/0b42e7ce8f79f2ebaba513e90ebf9a867b3d750b))
- *(video)* Skip Page.stopScreencast on already-closed page ([87b57a9](https://github.com/salamaashoush/ferridriver/commit/87b57a9e4949dbff4ce28389c4f5f3fd41f45c1e))
- *(server)* Exec user command so SIGTERM reaches the real child ([b6bd2d2](https://github.com/salamaashoush/ferridriver/commit/b6bd2d2aad9fcd1e831f02a09303ae005549d4fb))
- *(frame_cache)* Seed merges instead of replacing ([33059a0](https://github.com/salamaashoush/ferridriver/commit/33059a0a2255fc8b149696a04507213e93afda81))
- *(cdp)* Resolve missing iframe name via window.name on getFrameTree ([7cc85f9](https://github.com/salamaashoush/ferridriver/commit/7cc85f9eebc8c3ba17c89c8811a949d7e54c5ef7))
- *(error)* Classify timeout/target-closed Errs to typed variants ([de9dc57](https://github.com/salamaashoush/ferridriver/commit/de9dc572cca974b738904588968dd0baa591775e))
- *(locator)* Restore actionability-sentinel retry + radio message ([b235024](https://github.com/salamaashoush/ferridriver/commit/b23502441cd57a8e707b82d2ebf2b9b0214d75f8))
- *(mcp)* Serialize per-session tool calls + Playwright-shaped addCookies ([9873837](https://github.com/salamaashoush/ferridriver/commit/98738376e807bcbf02927655d107d5143851a953))
- *(webkit)* Migrate remaining Result<_, String> to typed FerriError ([64d243d](https://github.com/salamaashoush/ferridriver/commit/64d243dc2ab53b931b1c56b6f06efcb806da8551))
- *(bidi)* Deterministic elementHandle.contentFrame via contentWindow ([dc650d1](https://github.com/salamaashoush/ferridriver/commit/dc650d1d08ad166ed933d33ef8d445ef34fdd474))
- *(bidi)* Inject engine into freshly-attached / srcdoc / data: child contexts ([66c5c13](https://github.com/salamaashoush/ferridriver/commit/66c5c134e6980fb5173ba2505304614dd346cc8f))
- *(clippy)* Box oversized I/O futures; resolve bundle.rs lints ([43b79ac](https://github.com/salamaashoush/ferridriver/commit/43b79ac6783726416c411317f8d63c90abd58e72))
- *(justfile)* Portable in-place sed in the release recipe ([4cf3861](https://github.com/salamaashoush/ferridriver/commit/4cf3861281d0304a355657f984af321f61532a21))
- *(ci)* Write release notes to a file (inline --notes overflows arg limit) ([e65c3fc](https://github.com/salamaashoush/ferridriver/commit/e65c3fc79e72f2540d487e1a2a62850442ca6e5d))
- *(release)* Unbreak 0.2.0 — workspace-pin macro deps, doc link, publish order ([7bc4251](https://github.com/salamaashoush/ferridriver/commit/7bc4251be0a0300277bc65205cd0bd447fc0f42a))
- Make script/serializer JSON paths arbitrary-precision-safe ([a52aece](https://github.com/salamaashoush/ferridriver/commit/a52aece7809d607559de3ef97d68eb8493a1df28))
- *(toolchain)* Pin nightly — rolldown/oxc deps require it ([3557b8f](https://github.com/salamaashoush/ferridriver/commit/3557b8f0fad1cdd48ba417a351acc85eb0b54983))
- *(doc)* ExtensionRegistry intra-doc link -> code span (private item) ([7bd65b5](https://github.com/salamaashoush/ferridriver/commit/7bd65b585b43c4a7c1c6783fb067d32769684fd3))

### CI

- Explicit bun test timeout to match bunfig and avoid 5s default ([cb3eb0b](https://github.com/salamaashoush/ferridriver/commit/cb3eb0bb22570c7bb3f5848bac20facaa07af61c))
- 120s bun timeout, browser fixture 120s, Firefox runtime deps ([9fee881](https://github.com/salamaashoush/ferridriver/commit/9fee881c3e39f68f6def777714db61cd254739bb))
- Bump NAPI inner-test timeout to 120s, expand Firefox deps ([bf5d313](https://github.com/salamaashoush/ferridriver/commit/bf5d313bca2675f3b5d1f1ca645c4957ed1d36bf))
- Trim Firefox deps to non-Chrome ones to avoid t64 conflicts ([3e0e6fc](https://github.com/salamaashoush/ferridriver/commit/3e0e6fc51811dcad8c5a3e197c225511460048ed))
- Drop Firefox install — bidi suite hangs > 30 min when Firefox is present ([5786e78](https://github.com/salamaashoush/ferridriver/commit/5786e78924d2f7a535bf11516346216f2c652cd3))
- Drop redundant cdp-pipe/cdp-raw separate test steps ([4849a50](https://github.com/salamaashoush/ferridriver/commit/4849a502f193345360ee053885de0e6b23fdb77c))
- *(release)* Drop Windows CLI target; decouple GitHub release from crates.io ([0746f2c](https://github.com/salamaashoush/ferridriver/commit/0746f2c667443d4baeb5e47676f280e4ddc9f73a))
- *(release)* Add cross target to the pinned (nightly) toolchain ([5b33500](https://github.com/salamaashoush/ferridriver/commit/5b3350063fad7e10684c9d8b813ad4a53b011ba3))

### Documentation

- *(parity)* Mark ferridriver-specific helpers as non-Playwright ([6d5ef6d](https://github.com/salamaashoush/ferridriver/commit/6d5ef6d5c89f44f5cacd34f904f9a18896ceee68))
- Rewrite README + site for the Rust-only surface ([e59e900](https://github.com/salamaashoush/ferridriver/commit/e59e9007ad41dffb52ed898789e83ae672eec2ed))
- Survey plugin architectures; record adopt/defer decisions ([35ee457](https://github.com/salamaashoush/ferridriver/commit/35ee457e62ff9386d100aa1678511cfe91723af5))

### Features

- *(mcp)* Plugin system with QuickJS bindings + tool promotion ([d15d11a](https://github.com/salamaashoush/ferridriver/commit/d15d11aa1dbb5830a83158738efe3fef8fa8c26e))
- *(mcp)* One plugin file can declare multiple tools ([9545939](https://github.com/salamaashoush/ferridriver/commit/9545939b80f539c4b58ed4851ffd1409078619e6))
- *(core)* Sync Playwright accessors + predicate-route plumbing ([7393dc9](https://github.com/salamaashoush/ferridriver/commit/7393dc9635a0fe61285bea741a614d6b745ddd8f))
- *(script)* Playwright parity for QuickJS bindings ([233b5c6](https://github.com/salamaashoush/ferridriver/commit/233b5c6ea64095db52840e6478f97946ae01ecbe))
- *(node)* Playwright parity for NAPI bindings ([06f689a](https://github.com/salamaashoush/ferridriver/commit/06f689af7177e534995e9bbc31fe2f3ce0958cbe))
- *(mcp)* Plugin loader/server/script work-in-progress ([46efb69](https://github.com/salamaashoush/ferridriver/commit/46efb69366a2451d2d81efb89398bb00618acdcb))
- *(parity)* Mouse/keyboard delay option (Playwright {delay}) ([078d4b3](https://github.com/salamaashoush/ferridriver/commit/078d4b36bfba8b74de905e8d2d64eb4cdc93498d))
- *(parity)* Add Playwright page.ariaSnapshot; mark snapshotForAI non-Playwright ([d28bfcb](https://github.com/salamaashoush/ferridriver/commit/d28bfcb21289deacdf43eecacf7a0558acbf55c2))
- *(parity)* Locator.ariaSnapshot via vendored Playwright InjectedScript ([40cecbe](https://github.com/salamaashoush/ferridriver/commit/40cecbe1603e6f744a918d24fba7bc1ac969ac70))
- *(parity)* Cross-iframe stitching for locator.ariaSnapshot ([23e1b62](https://github.com/salamaashoush/ferridriver/commit/23e1b62ff30986640bd6b38dd3f3a864a39dfe18))
- *(bdd)* Run JavaScript step definitions on the shared QuickJS engine ([0b702d2](https://github.com/salamaashoush/ferridriver/commit/0b702d2cc68530866f800b0e3ab816f46d3b8f65))
- *(bdd)* Ferridriver bdd --steps runs JS step files through the core TestRunner ([d035f5f](https://github.com/salamaashoush/ferridriver/commit/d035f5f721b4ffe29f3920cc11e03b535faa8a94))
- *(bdd)* Rolldown front-end — TypeScript, tree-shaking, whole-graph bytecode ([c070e59](https://github.com/salamaashoush/ferridriver/commit/c070e5903bbdba8c1f66ca6a8e2808f6c8accf6d))
- *(plugin)* Migrate plugin loading to rolldown->bytecode pipeline ([0db489a](https://github.com/salamaashoush/ferridriver/commit/0db489a13b22bf9d039b08111acdcb4348ccd4db))
- *(plugin)* Declarative, default-deny capability manifest (exec + net) ([30771a4](https://github.com/salamaashoush/ferridriver/commit/30771a4bde7965df8c4b6b53c2cc6f457ffebb52))
- *(extension)* Test runner loads extensions; native ferridriver.host flag ([6bf90b2](https://github.com/salamaashoush/ferridriver/commit/6bf90b224cabaea5e0fe610aafc963dfa7d2c683))
- *(bdd)* Native this.attach / this.log -> reporters ([c6bb583](https://github.com/salamaashoush/ferridriver/commit/c6bb58387ecf7744e495bffaf586cd140d6f6007))
- *(bdd)* Hook callback arg (cucumber result/pickle) ([3a735f4](https://github.com/salamaashoush/ferridriver/commit/3a735f467e34c919888b108bb6f69a236cfab331))
- *(bdd)* DefineParameterType transformer (typed step args) ([2925a6c](https://github.com/salamaashoush/ferridriver/commit/2925a6c60c773182b08a53eb36c53c0b5dae4f17))
- *(bdd)* SetDefinitionFunctionWrapper + per-step/hook timeout ([213299e](https://github.com/salamaashoush/ferridriver/commit/213299e01e93c5eccf6a8025c46f6f54ff4a53df))
- *(bdd)* World parameters -> this.parameters (Tier 2 complete) ([4603f48](https://github.com/salamaashoush/ferridriver/commit/4603f48219bab9a284b4e9973fd450e172e72664))
- *(bdd)* This.skip() + JS undefined-step snippets (Tier 3) ([ac38c57](https://github.com/salamaashoush/ferridriver/commit/ac38c571628816d56663293ab1ef8a069b95c22b))

### Performance

- *(script)* Defer cycle-GC + skip redundant runtime limit setters ([c9d0a24](https://github.com/salamaashoush/ferridriver/commit/c9d0a244fee5fb4f2576834ced15f946d793014a))
- *(script)* Enable full-async; drop serde_json middle-hop on hot paths ([a5cbd34](https://github.com/salamaashoush/ferridriver/commit/a5cbd34363c63d078eb22f7a9535f38a20fbf3b9))
- *(script)* Kill remaining serde_json / JSON round-trips (standard path) ([38597fc](https://github.com/salamaashoush/ferridriver/commit/38597fce6589679c63c8a218884d8e4348eaa090))
- *(bdd)* Compile JS step files to bytecode once, link per worker ([ca7bac7](https://github.com/salamaashoush/ferridriver/commit/ca7bac74b04f33ca34b85a6393a5a8b10012f27d))
- *(plugin)* Content-hash bytecode cache, parallel bundling, parse-once install ([edb51bc](https://github.com/salamaashoush/ferridriver/commit/edb51bc5cfb64ad1f08ac3e5e20df3e0f641d2d4))

### Refactoring

- *(error)* Drop FerriError::Other escape hatch; route raw strings to typed Backend variant ([296da2e](https://github.com/salamaashoush/ferridriver/commit/296da2e7887df919f496e6dfff7e7114c2e08637))
- *(error)* Migrate backend wrapper, CDP/BiDi/WebKit impls, state/actions/selectors to FerriError ([c4a3406](https://github.com/salamaashoush/ferridriver/commit/c4a34069389c836d449f424e74cd8466361175ff))
- *(error)* Migrate BDD hook trait + macros to FerriError ([46b9240](https://github.com/salamaashoush/ferridriver/commit/46b92406a942f21922e52aa6482e16d2a24904f4))
- *(error)* Migrate Reporter trait + impls to FerriError ([a2445cb](https://github.com/salamaashoush/ferridriver/commit/a2445cb03f6b4c165da162b40b7951f54fff5973))
- *(error)* Migrate ShardArg::parse to FerriError ([71ecffe](https://github.com/salamaashoush/ferridriver/commit/71ecffe389f82b66c51189b6a7d5eed521428ddf))
- *(error)* Migrate NAPI test_runner JS-callback helpers to FerriError ([514f754](https://github.com/salamaashoush/ferridriver/commit/514f7540f6f7a0693d91717fab61f9f9af928361))
- *(error)* Migrate ferridriver core internals to FerriError ([ae37d91](https://github.com/salamaashoush/ferridriver/commit/ae37d91e0d6944479f03ce73c97a6fb40a71e7f6))
- *(error)* Migrate ferridriver-test internals to FerriError ([83169e2](https://github.com/salamaashoush/ferridriver/commit/83169e24ad894e73698aa4b872d8427e13c32384))
- *(error)* Migrate ferridriver-bdd parsers + finalise doc cleanup ([066240b](https://github.com/salamaashoush/ferridriver/commit/066240b5b21924e54b7329f2eb0ad6107e2b9c4e))
- *(error)* Drop redundant .map_err(|e| e.to_string()) bridges ([e5e7fa8](https://github.com/salamaashoush/ferridriver/commit/e5e7fa89d626232ff9a1824a391819217aac0d20))
- *(error)* Preserve typed FerriError prefix at reporter boundaries ([22a2f4b](https://github.com/salamaashoush/ferridriver/commit/22a2f4b44ad4d413ec03dff95b79843d0df9d326))
- *(error)* Centralize FerriError display + StepError typing ([9e2b667](https://github.com/salamaashoush/ferridriver/commit/9e2b6676642bdd9e46d83eeaf738f50555c63282))
- *(error)* Reclassify raw-string Errs to typed FerriError variants ([9137596](https://github.com/salamaashoush/ferridriver/commit/91375969fc13e1aa7731ca82c5cbf32524bf1347))
- *(node)* Slim ferridriver-node to a core-only browser binding ([80e96b4](https://github.com/salamaashoush/ferridriver/commit/80e96b430ca459f5d8533df42164deeca2d56103))
- Delete TS CLI + component-test packages; Rust-only surface ([e14b2a8](https://github.com/salamaashoush/ferridriver/commit/e14b2a8c263ccf1784393136a9de099209144e44))
- *(plugin)* Native ExtensionRegistry — kill all __ferri* JS shims ([4daad25](https://github.com/salamaashoush/ferridriver/commit/4daad25b7f9f7b9a38ebe423df6c4caf870133dd))
- *(page)* Native route registry — kill __fdRoutes/__fdRoutePreds ([b8a2a8d](https://github.com/salamaashoush/ferridriver/commit/b8a2a8d37bd62959ff8bdc1639035c8d7db443d1))
- Native page-callbacks registry + native Error — kill last __fd*/eval ([90dbfa1](https://github.com/salamaashoush/ferridriver/commit/90dbfa16656dad2fb8a6fbc2fde31baf130667f9))
- *(plugin)* Drop legacy globalThis.exports — defineTool is the only API ([8c41e40](https://github.com/salamaashoush/ferridriver/commit/8c41e405a1500a3261b37fdde21ee84520abcd34))
- *(config)* Un-nest plugins -> top-level `extensions` ([951a7f1](https://github.com/salamaashoush/ferridriver/commit/951a7f12d67dec6752db6b4c027630812b695467))

### Styling

- Cargo fmt for set_file_input edit ([96e7a3c](https://github.com/salamaashoush/ferridriver/commit/96e7a3c82125a36b0a02183da2a9562a73dd8e43))
- Cargo fmt for bidi name fallback ([b960e55](https://github.com/salamaashoush/ferridriver/commit/b960e559e78f259d1104a78054b5500a51b2b7d5))
- Cargo fmt ([506e3aa](https://github.com/salamaashoush/ferridriver/commit/506e3aa9a3a2564ad69ccd7d85e7bc663f5cf28c))
- Cargo fmt workspace ([015848e](https://github.com/salamaashoush/ferridriver/commit/015848e5f81e355ff860de112ce5e86ff48dbab9))

### Testing

- *(napi)* Default helper browser.headless=true for CI ([2a4856d](https://github.com/salamaashoush/ferridriver/commit/2a4856d9d9381433765749c2742bd307eff5ef4d))
- Force headless under CI env var ([d509698](https://github.com/salamaashoush/ferridriver/commit/d5096981a136ae2ff60f2eaaab1f4d78daf96f1f))
- *(napi)* WebSocket round-trip uses real http origin + null last expr ([c960b27](https://github.com/salamaashoush/ferridriver/commit/c960b27ff630aa3c270d8c9f72170489095922f2))
- *(cli)* WebSocket round-trip uses real http origin + null last expr ([b72f364](https://github.com/salamaashoush/ferridriver/commit/b72f3643e87f4f34919fe4158170317384cf475e))
- *(cli)* Exhaustive binding coverage + harness updates ([d574cfb](https://github.com/salamaashoush/ferridriver/commit/d574cfb82ab7384009236b3d431750b91cda97de))
- *(parity)* Nested + re-attached frame-locator coverage ([066e37e](https://github.com/salamaashoush/ferridriver/commit/066e37ec645c22db3564c93cd04681c74f6f81c3))
- *(download)* Deterministic cancel-surfaces-failure (no cancel race) ([741292a](https://github.com/salamaashoush/ferridriver/commit/741292a9c5fb7e47a947a5979ee305b8c88ff1bc))
- *(plugin)* Add re-runnable plugin-path microbench (ignored) ([1b9588b](https://github.com/salamaashoush/ferridriver/commit/1b9588b17fd702d6b816005c8f43f8c471ee95c3))

### Release

- V0.2.0 ([8a2e6d2](https://github.com/salamaashoush/ferridriver/commit/8a2e6d26b6fcaa0af06e31f81a5d3fd7bf47c766))

### Revert

- Timeout bumps and BiDi name-resolution sleeps ([a56506e](https://github.com/salamaashoush/ferridriver/commit/a56506e9aba0e83cf022f92ad498e38586a458b8))
- *(bidi)* Drop locateNodes name resolution; back to window.name only ([4aa4414](https://github.com/salamaashoush/ferridriver/commit/4aa44143245b1ec0c3f4cc0c89890edb37e116cf))


