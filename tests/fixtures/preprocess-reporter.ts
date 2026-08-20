// Edits the corpus before the run: drops one case, marks another
// skipped, and takes sharding over.

import type { ReporterPreprocessParams } from '@ferridriver/test';

export default class PreprocessReporter {
  private readonly outputFile: string;

  constructor(options: { outputFile?: string } = {}) {
    this.outputFile = options.outputFile ?? 'preprocess-reporter.json';
  }

  async preprocess({ suite, testRun }: ReporterPreprocessParams): Promise<void> {
    const tests = suite.allTests();
    const seen = tests.map((test) => test.title);
    const excluded = tests.find((test) => test.title.includes('excluded'));
    if (excluded) testRun.exclude(excluded);
    const skipped = tests.find((test) => test.title.includes('skipped'));
    if (skipped) testRun.skip(skipped, 'a reporter said so');
    testRun.skipSharding();
    await fs.promises.writeFile(this.outputFile, JSON.stringify({ seen }));
  }
}
