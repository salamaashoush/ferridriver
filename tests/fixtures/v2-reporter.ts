// The current (V2) reporter interface: `version()` answers 'v2', so
// `onConfigure` carries the config and `onBegin` takes the suite alone.

import type {
  ReporterFullConfig,
  ReporterFullResult,
  ReporterSuite,
  ReporterV2,
} from '@ferridriver/test';

export default class V2Reporter implements ReporterV2 {
  private readonly outputFile: string;
  private rootDir: string | undefined;
  private beganWith: string | undefined;
  private beganType: string | undefined;

  constructor(options: { outputFile?: string } = {}) {
    this.outputFile = options.outputFile ?? 'v2-reporter.json';
  }

  version(): 'v2' {
    return 'v2';
  }

  onConfigure(config: ReporterFullConfig): void {
    this.rootDir = config.rootDir;
  }

  onBegin(suite: ReporterSuite): void {
    this.beganType = suite.type;
    this.beganWith = suite.allTests().length.toString();
  }

  async onEnd(_result: ReporterFullResult): Promise<void> {
    await fs.writeFile(
      this.outputFile,
      JSON.stringify({ rootDir: this.rootDir, beganType: this.beganType, beganWith: this.beganWith })
    );
  }
}
