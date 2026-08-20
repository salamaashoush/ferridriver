// A reporter written against the ORIGINAL (V1) interface — the one
// `@playwright/test` documents and third-party reporters implement. It
// declares no `version()`, so `onBegin` takes the config first and
// `onConfigure` is never called.
//
// Every hook is counted and the whole picture is written as JSON in
// `onEnd`, so a test can assert on what the reporter was actually
// handed rather than on the fact that it did not throw.

import type {
  Reporter,
  ReporterFullConfig,
  ReporterFullResult,
  ReporterSuite,
  ReporterTestCase,
  ReporterTestResult,
  ReporterTestStep,
} from '@ferridriver/test';

interface Options {
  outputFile?: string;
  configDir?: string;
  printsToStdio?: boolean;
}

export default class CountingReporter implements Reporter {
  private readonly options: Options;
  private readonly calls: Record<string, number> = {};
  private config: ReporterFullConfig | undefined;
  private suite: ReporterSuite | undefined;
  private readonly statuses: string[] = [];
  private readonly outcomes: string[] = [];
  private readonly ok: boolean[] = [];
  private readonly stepTitles: string[] = [];
  private readonly stepPaths: string[][] = [];
  private readonly attachments: { name: string; contentType: string; body?: string; path?: string }[] = [];
  private readonly stdout: string[] = [];
  private readonly errors: string[] = [];
  private configuredCalled = false;

  constructor(options: Options = {}) {
    this.options = options;
  }

  private count(hook: string): void {
    this.calls[hook] = (this.calls[hook] ?? 0) + 1;
  }

  // A V1 reporter must never see this; recording it is how the test
  // proves the wrapper kept the two interfaces apart.
  onConfigure(config: ReporterFullConfig): void {
    this.configuredCalled = true;
    void config;
  }

  onBegin(config: ReporterFullConfig, suite: ReporterSuite): void {
    this.count('onBegin');
    this.config = config;
    this.suite = suite;
  }

  onTestBegin(test: ReporterTestCase, result: ReporterTestResult): void {
    this.count('onTestBegin');
    // `status` is undefined until the attempt ends, as upstream.
    if (result.status !== undefined) this.errors.push(`onTestBegin saw status ${String(result.status)}`);
    if (!test.results.includes(result)) this.errors.push('onTestBegin result is not on the case');
  }

  onStepBegin(_test: ReporterTestCase, _result: ReporterTestResult, step: ReporterTestStep): void {
    this.count('onStepBegin');
    this.stepTitles.push(step.title);
    this.stepPaths.push(step.titlePath());
  }

  onStepEnd(_test: ReporterTestCase, _result: ReporterTestResult, step: ReporterTestStep): void {
    this.count('onStepEnd');
    if (step.error) this.errors.push(`step failed: ${step.error.message ?? ''}`);
  }

  onStdOut(chunk: string): void {
    this.count('onStdOut');
    this.stdout.push(chunk);
  }

  onStdErr(chunk: string): void {
    this.count('onStdErr');
    this.stdout.push(chunk);
  }

  onTestEnd(test: ReporterTestCase, result: ReporterTestResult): void {
    this.count('onTestEnd');
    this.statuses.push(String(result.status));
    this.outcomes.push(test.outcome());
    this.ok.push(test.ok());
    for (const attachment of result.attachments) {
      this.attachments.push({
        name: attachment.name,
        contentType: attachment.contentType,
        body: attachment.body ? attachment.body.toString('base64') : undefined,
        path: attachment.path,
      });
    }
  }

  onError(error: { message?: string }): void {
    this.count('onError');
    this.errors.push(error.message ?? '');
  }

  async onEnd(result: ReporterFullResult): Promise<void> {
    this.count('onEnd');
    if (this.options.printsToStdio) console.log('counting-reporter printed');
    const summary = {
      calls: this.calls,
      configuredCalled: this.configuredCalled,
      configRootDir: this.config?.rootDir,
      configProjects: (this.config?.projects ?? []).map((project) => project.name),
      suiteType: this.suite?.type,
      suiteTitlePath: this.suite?.titlePath(),
      allTests: (this.suite?.allTests() ?? []).map((test) => test.titlePath().join(' > ')),
      entryTypes: (this.suite?.entries() ?? []).map((entry) => entry.type),
      firstProject: this.suite?.suites[0]?.project()?.name,
      testProject: this.suite?.allTests()[0]?.parent.parent?.project()?.name,
      statuses: this.statuses,
      outcomes: this.outcomes,
      ok: this.ok,
      stepTitles: this.stepTitles,
      stepPaths: this.stepPaths,
      attachments: this.attachments,
      stdout: this.stdout,
      errors: this.errors,
      status: result.status,
      durationIsNumber: typeof result.duration === 'number',
      startTimeIsDate: result.startTime instanceof Date,
    };
    const target = this.options.outputFile ?? 'counting-reporter.json';
    await fs.promises.writeFile(target, JSON.stringify(summary, null, 2));
  }

  printsToStdio(): boolean {
    return this.options.printsToStdio === true;
  }
}
