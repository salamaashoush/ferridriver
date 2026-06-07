//! Source for the virtual runtime modules exposed to scripts and
//! bundled extensions.

pub const FERRIDRIVER_MODULE: &str = r"
const fd = globalThis.ferridriver;
export default fd;
export const ferridriver = fd;
export const host = fd.host;
export const tool = fd.tool;
export const defineTool = fd.tool;
export const bdd = fd.bdd;
export const commands = fd.commands;
export const tools = fd.tools;
export const fs = fd.fs;
export const vars = fd.vars;
export const sidecars = fd.sidecars;
export const artifacts = fd.artifacts;
export const page = globalThis.page;
export const context = globalThis.context;
export const browser = globalThis.browser;
export const request = globalThis.request;
export const expect = globalThis.expect;
export const chromium = globalThis.chromium;
export const firefox = globalThis.firefox;
export const webkit = globalThis.webkit;
";

pub const CUCUMBER_MODULE: &str = r"
const bdd = globalThis.ferridriver.bdd;
export const Given = bdd.Given;
export const When = bdd.When;
export const Then = bdd.Then;
export const defineStep = bdd.defineStep;
export const And = bdd.And;
export const But = bdd.But;
export const Before = bdd.Before;
export const After = bdd.After;
export const BeforeAll = bdd.BeforeAll;
export const AfterAll = bdd.AfterAll;
export const BeforeStep = bdd.BeforeStep;
export const AfterStep = bdd.AfterStep;
export const defineParameterType = bdd.defineParameterType;
export const setDefaultTimeout = bdd.setDefaultTimeout;
export const setDefinitionFunctionWrapper = bdd.setDefinitionFunctionWrapper;
export const setWorldConstructor = bdd.setWorldConstructor;
export const setParallelCanAssign = bdd.setParallelCanAssign;
";
