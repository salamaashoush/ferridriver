import { describe, it, expect } from "bun:test";
import {
  TimeoutError,
  TargetClosedError,
  isTimeoutError,
  isTargetClosedError,
  promoteError,
  withPromotedErrors,
  errors,
} from "../src/errors.js";

describe("error classes", () => {
  it("TimeoutError carries name and message", () => {
    const e = new TimeoutError("Timeout 30000ms exceeded");
    expect(e).toBeInstanceOf(TimeoutError);
    expect(e).toBeInstanceOf(Error);
    expect(e.name).toBe("TimeoutError");
    expect(e.message).toBe("Timeout 30000ms exceeded");
  });

  it("TargetClosedError carries name and message", () => {
    const e = new TargetClosedError("Target page, context or browser has been closed");
    expect(e).toBeInstanceOf(TargetClosedError);
    expect(e).toBeInstanceOf(Error);
    expect(e.name).toBe("TargetClosedError");
  });

  it("errors namespace matches Playwright shape", () => {
    expect(errors.TimeoutError).toBe(TimeoutError);
    expect(errors.TargetClosedError).toBe(TargetClosedError);
  });
});

describe("type predicates", () => {
  it("isTimeoutError matches class instance", () => {
    expect(isTimeoutError(new TimeoutError("x"))).toBe(true);
    expect(isTimeoutError(new Error("x"))).toBe(false);
    expect(isTimeoutError("string")).toBe(false);
    expect(isTimeoutError(null)).toBe(false);
  });

  it("isTimeoutError also matches Errors named TimeoutError for cross-realm safety", () => {
    const plain = new Error("msg");
    Object.defineProperty(plain, "name", { value: "TimeoutError" });
    expect(isTimeoutError(plain)).toBe(true);
  });

  it("isTargetClosedError matches class and name", () => {
    expect(isTargetClosedError(new TargetClosedError("x"))).toBe(true);
    expect(isTargetClosedError(new Error("y"))).toBe(false);
    const plain = new Error("msg");
    Object.defineProperty(plain, "name", { value: "TargetClosedError" });
    expect(isTargetClosedError(plain)).toBe(true);
  });
});

describe("promoteError", () => {
  it("promotes tagged TimeoutError NAPI message to class instance", () => {
    const napiErr = new Error("TimeoutError: Timeout 30000ms exceeded while navigating");
    const promoted = promoteError(napiErr);
    expect(promoted).toBeInstanceOf(TimeoutError);
    expect(promoted.name).toBe("TimeoutError");
    expect(promoted.message).toBe("Timeout 30000ms exceeded while navigating");
  });

  it("promotes tagged TargetClosedError NAPI message to class instance", () => {
    const napiErr = new Error("TargetClosedError: Target page, context or browser has been closed: browser crashed");
    const promoted = promoteError(napiErr);
    expect(promoted).toBeInstanceOf(TargetClosedError);
    expect(promoted.message).toBe("Target page, context or browser has been closed: browser crashed");
  });

  it("preserves the stack trace when promoting", () => {
    const napiErr = new Error("TimeoutError: Timeout 1000ms exceeded");
    const original = napiErr.stack;
    const promoted = promoteError(napiErr);
    expect(promoted.stack).toBe(original);
  });

  it("returns untagged errors unchanged", () => {
    const plain = new Error("backend error: launch failed");
    const out = promoteError(plain);
    expect(out).toBe(plain);
    expect(out).not.toBeInstanceOf(TimeoutError);
  });

  it("returns already-promoted errors unchanged", () => {
    const already = new TimeoutError("Timeout 1ms exceeded");
    expect(promoteError(already)).toBe(already);
  });

  it("wraps non-Error throwables in a plain Error", () => {
    const out = promoteError("just a string");
    expect(out).toBeInstanceOf(Error);
    expect(out.message).toBe("just a string");
  });
});

describe("withPromotedErrors", () => {
  it("resolves with the callback's value when it succeeds", async () => {
    const v = await withPromotedErrors(async () => 42);
    expect(v).toBe(42);
  });

  it("rethrows promoted errors for tagged NAPI messages", async () => {
    const fn = async () => {
      throw new Error("TimeoutError: Timeout 100ms exceeded");
    };
    let caught: unknown;
    try {
      await withPromotedErrors(fn);
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(TimeoutError);
  });

  it("rethrows untagged errors unchanged", async () => {
    const original = new Error("random");
    const fn = async () => {
      throw original;
    };
    let caught: unknown;
    try {
      await withPromotedErrors(fn);
    } catch (e) {
      caught = e;
    }
    expect(caught).toBe(original);
  });
});
