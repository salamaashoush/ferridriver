// `onEnd` returning `{ status }` decides how the run is reported to
// have ended, and with it the exit code.

import type { ReporterFullResult } from '@ferridriver/test';

export default class StatusReporter {
  private readonly status: ReporterFullResult['status'];

  constructor(options: { status?: ReporterFullResult['status'] } = {}) {
    this.status = options.status ?? 'passed';
  }

  onEnd(_result: ReporterFullResult): { status: ReporterFullResult['status'] } {
    return { status: this.status };
  }
}
