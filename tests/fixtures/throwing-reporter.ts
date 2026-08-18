// Every hook throws. The run must still finish and every other
// reporter must still be driven — Playwright's multiplexer catches a
// reporter callback rather than letting it take the run down.

export default class ThrowingReporter {
  onBegin(): void {
    throw new Error('onBegin exploded');
  }
  onTestBegin(): void {
    throw new Error('onTestBegin exploded');
  }
  onTestEnd(): void {
    throw new Error('onTestEnd exploded');
  }
  onStepBegin(): void {
    throw new Error('onStepBegin exploded');
  }
  onStepEnd(): void {
    throw new Error('onStepEnd exploded');
  }
  onEnd(): void {
    throw new Error('onEnd exploded');
  }
}
